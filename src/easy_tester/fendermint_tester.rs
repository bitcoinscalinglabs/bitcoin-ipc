use std::collections::HashMap;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use alloy_primitives::U256;
use log::{debug, info};

use crate::easy_tester::error::EasyTesterError;
use crate::easy_tester::model::{FendermintSetup, OutputExpectTarget};
use crate::easy_tester::OutputDb;
use crate::easy_tester::ScenarioCommand;

const IPC_CLI_CONFIG: &str = "/root/.ipc/validator1/config.toml";
const POLL_INTERVAL_SECS: u64 = 5;
const POLL_TIMEOUT_SECS: u64 = 90;
const DECIMALS: u32 = 18;

/// Convert a user-provided localhost URL to the container-internal form.
/// Inside Docker, services running on the host are reached via `host.docker.internal`.
fn to_container_url(url: &str) -> String {
    url.replace("localhost", "host.docker.internal")
        .replace("127.0.0.1", "host.docker.internal")
}

fn ten_pow_decimals() -> U256 {
    U256::from(10u64).pow(U256::from(DECIMALS))
}

#[derive(Debug, Clone)]
struct TokenRegistration {
    home_subnet: String,
    home_address: String,
    issuer: String,
}

#[derive(Debug, Clone)]
struct DiscoveredSubnet {
    subnet_id: String,
    gateway_address: String,
    /// ETH RPC URL as seen from inside the container (host.docker.internal)
    eth_rpc_url: String,
    _provider_url: String,
}

pub struct FendermintTester {
    docker_container: String,
    setup: FendermintSetup,
    print_queries: bool,

    // Discovered at startup
    issuer_private_keys: HashMap<String, String>,
    discovered_subnets: HashMap<String, DiscoveredSubnet>,

    // Token state
    registered_tokens: HashMap<String, TokenRegistration>,
    wrapped_addresses: HashMap<(String, String), String>, // (token_name, subnet_name) -> addr

    // Last read results (+ args for retry in expect)
    last_token_balance: Option<U256>,
    last_token_metadata: Option<HashMap<String, String>>,
    last_read_args: Option<(OutputDb, Vec<String>)>,
}

impl FendermintTester {
    pub fn new(setup: FendermintSetup) -> Result<Self, EasyTesterError> {
        let docker = &setup.docker_container;

        info!("FendermintTester: discovering issuer keys and subnet IDs...");
        info!("FendermintTester: NOPed commands: block, checkpoint");
        info!("FendermintTester: unsupported commands: create, join, stake, unstake");

        // 1. Discover issuer private keys
        let wallet_output = docker_exec(
            docker,
            &[
                "ipc-cli",
                "--config-path",
                IPC_CLI_CONFIG,
                "wallet",
                "list",
                "--wallet-type",
                "btc",
            ],
        )?;
        let mut issuer_private_keys = HashMap::new();
        for issuer in setup.issuers.values() {
            let pk = parse_private_key_from_wallet_list(&wallet_output, &issuer.ipc_address)
                .map_err(|e| {
                    EasyTesterError::runtime(format!(
                        "could not find private key for issuer '{}' ({}): {e}",
                        issuer.name, issuer.ipc_address
                    ))
                })?;
            info!(
                "  Issuer '{}' ({}) — private key discovered",
                issuer.name, issuer.ipc_address
            );
            issuer_private_keys.insert(issuer.name.clone(), pk);
        }

        // 2. Discover gateway addresses from config.toml using the known subnet IDs
        let config_toml = docker_exec(docker, &["cat", IPC_CLI_CONFIG])?;
        let config_entries = parse_fevm_subnets_from_config(&config_toml);

        let mut discovered_subnets = HashMap::new();
        for subnet in setup.subnets.values() {
            let gateway_address = config_entries
                .iter()
                .find(|(id, _, _)| *id == subnet.subnet_id)
                .map(|(_, _, gw)| gw.clone())
                .ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "subnet '{}' (id={}) not found in config.toml — \
                         is the subnet ID correct?",
                        subnet.name, subnet.subnet_id
                    ))
                })?;

            info!(
                "  Subnet '{}' → id={}, gateway={}",
                subnet.name, subnet.subnet_id, gateway_address
            );
            discovered_subnets.insert(
                subnet.name.clone(),
                DiscoveredSubnet {
                    subnet_id: subnet.subnet_id.clone(),
                    gateway_address,
                    eth_rpc_url: to_container_url(&subnet.eth_rpc_url),
                    _provider_url: subnet.provider_url.clone(),
                },
            );
        }

        // 3. Poll liveness
        for subnet in setup.subnets.values() {
            let ds = &discovered_subnets[&subnet.name];
            poll_block_advancing(&setup.docker_container, &ds.eth_rpc_url)?;
            info!("  Subnet '{}' — blocks advancing", subnet.name);
        }

        info!("FendermintTester: ready");

        Ok(Self {
            docker_container: setup.docker_container.clone(),
            print_queries: setup.print_queries,
            setup,
            issuer_private_keys,
            discovered_subnets,
            registered_tokens: HashMap::new(),
            wrapped_addresses: HashMap::new(),
            last_token_balance: None,
            last_token_metadata: None,
            last_read_args: None,
        })
    }

    pub fn run(&mut self, scenario: Vec<(usize, ScenarioCommand)>) -> Result<(), EasyTesterError> {
        for (line_no, cmd) in scenario {
            let annotate = |e: EasyTesterError| -> EasyTesterError {
                EasyTesterError::runtime(format!("line {line_no}: {e}"))
            };

            match cmd {
                ScenarioCommand::Block { .. } | ScenarioCommand::Checkpoint { .. } => {
                    // NOP
                }
                ScenarioCommand::Wait { seconds } => {
                    println!("line {}: Waiting {} seconds...", line_no, seconds);
                    thread::sleep(Duration::from_secs(seconds));
                }
                ScenarioCommand::Create { .. }
                | ScenarioCommand::Join { .. }
                | ScenarioCommand::Stake { .. }
                | ScenarioCommand::Unstake { .. } => {
                    return Err(annotate(EasyTesterError::runtime(
                        "create/join/stake/unstake not supported by FendermintTester",
                    )));
                }
                ScenarioCommand::RegisterToken {
                    subnet_name,
                    issuer,
                    name,
                    symbol,
                    initial_supply,
                } => {
                    let issuer_name = issuer.ok_or_else(|| {
                        annotate(EasyTesterError::runtime(
                            "register_token requires an issuer in FendermintTester",
                        ))
                    })?;
                    self.exec_register_token(
                        line_no,
                        &subnet_name,
                        &issuer_name,
                        &name,
                        &symbol,
                        initial_supply,
                    )
                    .map_err(&annotate)?;
                }
                ScenarioCommand::MintToken {
                    subnet_name,
                    token_name,
                    amount,
                } => {
                    self.exec_mint_token(&subnet_name, &token_name, amount)
                        .map_err(&annotate)?;
                }
                ScenarioCommand::BurnToken {
                    subnet_name,
                    token_name,
                    amount,
                } => {
                    self.exec_burn_token(&subnet_name, &token_name, amount)
                        .map_err(&annotate)?;
                }
                ScenarioCommand::Deposit {
                    subnet_name,
                    address_name,
                    amount_sats,
                } => {
                    self.exec_deposit(line_no, &subnet_name, &address_name, amount_sats)
                        .map_err(&annotate)?;
                }
                ScenarioCommand::ErcTransfer {
                    src_subnet,
                    src_actor,
                    dst_subnet,
                    dst_actor,
                    token_name,
                    amount,
                } => {
                    let src_a = src_actor.ok_or_else(|| {
                        annotate(EasyTesterError::runtime(
                            "erc_transfer requires src_actor in FendermintTester",
                        ))
                    })?;
                    let dst_a = dst_actor.ok_or_else(|| {
                        annotate(EasyTesterError::runtime(
                            "erc_transfer requires dst_actor in FendermintTester",
                        ))
                    })?;
                    self.exec_erc_transfer(
                        &src_subnet,
                        &src_a,
                        &dst_subnet,
                        &dst_a,
                        &token_name,
                        amount,
                    )
                    .map_err(&annotate)?;
                }
                ScenarioCommand::OutputRead { db, args } => {
                    self.exec_output_read(line_no, db, &args)
                        .map_err(&annotate)?;
                }
                ScenarioCommand::OutputExpect {
                    target,
                    expected_value,
                } => {
                    let deadline = Instant::now() + Duration::from_secs(POLL_TIMEOUT_SECS);
                    loop {
                        match self.exec_output_expect(target.clone(), &expected_value) {
                            Ok(msg) => {
                                println!("OUTPUT expect {} \x1b[32m(ok)\x1b[0m", msg);
                                break;
                            }
                            Err(e) if Instant::now() < deadline => {
                                if let Some((db, ref args)) = self.last_read_args.clone() {
                                    debug!("expect mismatch, retrying read+expect in {}s... ({})", POLL_INTERVAL_SECS, e);
                                    thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
                                    self.exec_output_read(line_no, db, &args)
                                        .map_err(&annotate)?;
                                    continue;
                                }
                                return Err(EasyTesterError::runtime(format!(
                                    "line {line_no}: {} \x1b[31m(fail)\x1b[0m",
                                    e
                                )));
                            }
                            Err(e) => {
                                return Err(EasyTesterError::runtime(format!(
                                    "line {line_no}: {} \x1b[31m(fail)\x1b[0m",
                                    e
                                )));
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // Wrapper around free docker_exec that prints the command when
    // `print_queries` is enabled in the config. Use this for ipc-cli and
    // related queries that the user wants to observe.
    fn docker_exec_with_print(&self, args: &[&str]) -> Result<String, EasyTesterError> {
        if self.print_queries {
            println!("IPC query: docker exec {} {}", self.docker_container, args.join(" "));
        }
        docker_exec(&self.docker_container, args)
    }

    // ── Command implementations ────────────────────────────────────────

    fn exec_register_token(
        &mut self,
        line_no: usize,
        subnet_name: &str,
        issuer_name: &str,
        token_name: &str,
        symbol: &str,
        initial_supply: U256,
    ) -> Result<(), EasyTesterError> {
        let ds = self
            .discovered_subnets
            .get(subnet_name)
            .ok_or_else(|| EasyTesterError::runtime(format!("subnet '{subnet_name}' not found")))?;
        let private_key = self.issuer_private_keys.get(issuer_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("issuer '{issuer_name}' key not found"))
        })?;

        let raw_supply = initial_supply
            .checked_mul(ten_pow_decimals())
            .ok_or_else(|| {
                EasyTesterError::runtime("initial_supply * 10^18 overflow".to_string())
            })?;

        info!(
            "register_token: deploying {} ({}) on {} with supply {} (raw {})",
            token_name, symbol, subnet_name, initial_supply, raw_supply
        );

        let deploy_script = "/workspace/ipc/scripts/deploy_bridgeable_token.sh";
        println!(
            "line {}: Registered {} using script {}",
            line_no, token_name, deploy_script
        );

        let output = self.docker_exec_with_print(
            &[
                "bash",
                deploy_script,
                "--name",
                token_name,
                "--symbol",
                symbol,
                "--decimals",
                &DECIMALS.to_string(),
                "--initial-supply",
                &raw_supply.to_string(),
                "--rpc-url",
                &ds.eth_rpc_url,
                "--private-key",
                private_key,
                "--gateway",
                &ds.gateway_address,
                "--broadcast",
            ],
        )?;

        let token_address = parse_token_address_from_deploy_output(&output).map_err(|e| {
            EasyTesterError::runtime(format!(
                "failed to parse token address from deploy output: {e}\noutput:\n{output}"
            ))
        })?;

        info!(
            "register_token: {} deployed at {}",
            token_name, token_address
        );

        self.registered_tokens.insert(
            token_name.to_string(),
            TokenRegistration {
                home_subnet: subnet_name.to_string(),
                home_address: token_address.clone(),
                issuer: issuer_name.to_string(),
            },
        );

        Ok(())
    }

    fn exec_mint_token(
        &mut self,
        subnet_name: &str,
        token_name: &str,
        amount: U256,
    ) -> Result<(), EasyTesterError> {
        let reg = self
            .registered_tokens
            .get(token_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("token '{token_name}' not registered"))
            })?
            .clone();

        if subnet_name != reg.home_subnet {
            return Err(EasyTesterError::runtime(format!(
                "mint only supported on home subnet '{}', got '{subnet_name}'",
                reg.home_subnet
            )));
        }

        let ds = &self.discovered_subnets[subnet_name];
        let private_key = &self.issuer_private_keys[&reg.issuer];
        let issuer_addr = &self.setup.issuers[&reg.issuer].ipc_address;
        let raw_amount = amount
            .checked_mul(ten_pow_decimals())
            .ok_or_else(|| EasyTesterError::runtime("amount * 10^18 overflow".to_string()))?;

        info!(
            "mint_token: minting {} {} (raw {}) on {}",
            amount, token_name, raw_amount, subnet_name
        );

        self.docker_exec_with_print(&[
            "cast",
            "send",
            &reg.home_address,
            "mint(address,uint256)",
            issuer_addr,
            &raw_amount.to_string(),
            "--rpc-url",
            &ds.eth_rpc_url,
            "--private-key",
            private_key,
        ])?;

        Ok(())
    }

    fn exec_burn_token(
        &mut self,
        subnet_name: &str,
        token_name: &str,
        amount: U256,
    ) -> Result<(), EasyTesterError> {
        let reg = self
            .registered_tokens
            .get(token_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("token '{token_name}' not registered"))
            })?
            .clone();

        if subnet_name != reg.home_subnet {
            return Err(EasyTesterError::runtime(format!(
                "burn only supported on home subnet '{}', got '{subnet_name}'",
                reg.home_subnet
            )));
        }

        let ds = &self.discovered_subnets[subnet_name];
        let private_key = &self.issuer_private_keys[&reg.issuer];
        let issuer_addr = &self.setup.issuers[&reg.issuer].ipc_address;
        let raw_amount = amount
            .checked_mul(ten_pow_decimals())
            .ok_or_else(|| EasyTesterError::runtime("amount * 10^18 overflow".to_string()))?;

        info!(
            "burn_token: burning {} {} (raw {}) on {}",
            amount, token_name, raw_amount, subnet_name
        );

        self.docker_exec_with_print(&[
            "cast",
            "send",
            &reg.home_address,
            "burnFrom(address,uint256)",
            issuer_addr,
            &raw_amount.to_string(),
            "--rpc-url",
            &ds.eth_rpc_url,
            "--private-key",
            private_key,
        ])?;

        Ok(())
    }

    fn exec_deposit(
        &mut self,
        line_no: usize,
        subnet_name: &str,
        address_name: &str,
        amount_sats: u64,
    ) -> Result<(), EasyTesterError> {
        let ds = self
            .discovered_subnets
            .get(subnet_name)
            .ok_or_else(|| EasyTesterError::runtime(format!("subnet '{subnet_name}' not found")))?;

        let to_addr = &self
            .setup
            .issuers
            .get(address_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("address '{address_name}' not found"))
            })?
            .ipc_address;

        println!(
            "line {}: Deposited {} sats to {} on {}",
            line_no, amount_sats, address_name, subnet_name
        );

        self.docker_exec_with_print(&[
            "ipc-cli",
            "--config-path",
            IPC_CLI_CONFIG,
            "cross-msg",
            "fund",
            "--subnet",
            &ds.subnet_id,
            "btc",
            "--to",
            to_addr,
            &amount_sats.to_string(),
        ])?;

        Ok(())
    }

    fn exec_erc_transfer(
        &mut self,
        src_subnet: &str,
        src_actor: &str,
        dst_subnet: &str,
        dst_actor: &str,
        token_name: &str,
        amount: U256,
    ) -> Result<(), EasyTesterError> {
        let reg = self
            .registered_tokens
            .get(token_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("token '{token_name}' not registered"))
            })?
            .clone();

        let token_addr_on_src = self.resolve_token_address(token_name, src_subnet)?;

        let src_ds = self
            .discovered_subnets
            .get(src_subnet)
            .ok_or_else(|| EasyTesterError::runtime(format!("subnet '{src_subnet}' not found")))?
            .clone();
        let dst_ds = self
            .discovered_subnets
            .get(dst_subnet)
            .ok_or_else(|| EasyTesterError::runtime(format!("subnet '{dst_subnet}' not found")))?
            .clone();

        let src_addr = &self
            .setup
            .issuers
            .get(src_actor)
            .ok_or_else(|| EasyTesterError::runtime(format!("actor '{src_actor}' not found")))?
            .ipc_address
            .clone();
        let dst_addr = &self
            .setup
            .issuers
            .get(dst_actor)
            .ok_or_else(|| EasyTesterError::runtime(format!("actor '{dst_actor}' not found")))?
            .ipc_address
            .clone();

        let raw_amount = amount
            .checked_mul(ten_pow_decimals())
            .ok_or_else(|| EasyTesterError::runtime("amount * 10^18 overflow".to_string()))?;

        info!(
            "erc_transfer: {} {} from {}@{} to {}@{} (raw {})",
            amount, token_name, src_actor, src_subnet, dst_actor, dst_subnet, raw_amount
        );

        self.docker_exec_with_print(
            &[
                "ipc-cli",
                "--config-path",
                IPC_CLI_CONFIG,
                "cross-msg",
                "transfer-erc",
                "--source-subnet",
                &src_ds.subnet_id,
                "--destination-subnet",
                &dst_ds.subnet_id,
                "--source-address",
                src_addr,
                "--destination-address",
                dst_addr,
                "--token",
                &token_addr_on_src,
                &raw_amount.to_string(),
            ],
        )?;

        // Eagerly try to cache the wrapped address on dst_subnet
        if dst_subnet != reg.home_subnet {
            if let Err(e) = self.query_and_cache_metadata(token_name, dst_subnet) {
                debug!("erc_transfer: eager metadata query for {} on {} failed (will retry later): {e}", token_name, dst_subnet);
            }
        }

        Ok(())
    }

    fn exec_output_read(
        &mut self,
        line_no: usize,
        db: OutputDb,
        args: &[String],
    ) -> Result<(), EasyTesterError> {
        self.last_read_args = Some((db, args.to_vec()));
        match db {
            OutputDb::TokenBalance => {
                // 3-arg: <subnet> <actor> <token>
                if args.len() != 3 {
                    return Err(EasyTesterError::runtime(
                        "FendermintTester: read token_balance requires 3 args: <subnet> <actor> <token>",
                    ));
                }
                let subnet = &args[0];
                let actor = &args[1];
                let token_name = &args[2];

                let actor_addr = &self
                    .setup
                    .issuers
                    .get(actor.as_str())
                    .ok_or_else(|| EasyTesterError::runtime(format!("actor '{actor}' not found")))?
                    .ipc_address
                    .clone();

                let ds = self
                    .discovered_subnets
                    .get(subnet.as_str())
                    .ok_or_else(|| {
                        EasyTesterError::runtime(format!("subnet '{subnet}' not found"))
                    })?
                    .clone();

                // Resolve token address (may need metadata query for non-home subnets).
                // This polls because the wrapped token contract might not be deployed yet.
                println!(
                    "line {}: Reading {} balance of {} on {}...",
                    line_no, token_name, actor, subnet
                );
                let deadline = Instant::now() + Duration::from_secs(POLL_TIMEOUT_SECS);
                let mut attempt = 0u32;
                let token_addr = loop {
                    match self.resolve_token_address(token_name, subnet) {
                        Ok(addr) => break addr,
                        Err(e) if Instant::now() < deadline => {
                            info!("read token_balance: wrapped address not yet available (attempt {}), retrying in {}s... ({})", attempt, POLL_INTERVAL_SECS, e);
                            thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
                            attempt += 1;
                        }
                        Err(e) => return Err(e),
                    }
                };

                let output = self.docker_exec_with_print(
                    &[
                        "cast",
                        "call",
                        &token_addr,
                        "balanceOf(address)(uint256)",
                        &actor_addr,
                        "--rpc-url",
                        &ds.eth_rpc_url,
                    ],
                )?;

                let balance = parse_u256_from_cast_output(&output).map_err(|e| {
                    EasyTesterError::runtime(format!(
                        "failed to parse balanceOf output: {e}\nraw: {output}"
                    ))
                })?;

                info!(
                    "read token_balance: {} {} on {} = {} (raw)",
                    actor, token_name, subnet, balance
                );
                self.last_token_balance = Some(balance);
                self.last_token_metadata = None;
                Ok(())
            }
            OutputDb::TokenMetadata => {
                if args.len() != 2 {
                    return Err(EasyTesterError::runtime(
                        "FendermintTester: read token_metadata requires 2 args: <subnet> <token>",
                    ));
                }
                let subnet = &args[0];
                let token_name = &args[1];

                let metadata = self.query_metadata(token_name, subnet)?;
                info!(
                    "read token_metadata: {} on {} = {:?}",
                    token_name, subnet, metadata
                );
                self.last_token_metadata = Some(metadata);
                self.last_token_balance = None;
                Ok(())
            }
            other => {
                info!(
                    "Ignoring read {:?} (not supported by FendermintTester)",
                    other
                );
                self.last_token_balance = None;
                self.last_token_metadata = None;
                Ok(())
            }
        }
    }

    fn exec_output_expect(
        &self,
        target: OutputExpectTarget,
        expected_value: &str,
    ) -> Result<String, EasyTesterError> {
        // Token balance
        if let Some(balance) = &self.last_token_balance {
            if target.path == "balance" {
                if expected_value == "__not_empty__" {
                    if *balance == U256::ZERO {
                        return Err(EasyTesterError::runtime(
                            "expected non-empty balance, got 0".to_string(),
                        ));
                    }
                    return Ok(format!("balance.not_empty (got {})", balance));
                }

                let expected =
                    crate::easy_tester::model::parse_u256_allow_underscores(expected_value)
                        .map_err(|e| {
                            EasyTesterError::runtime(format!("invalid expected balance: {e}"))
                        })?;
                let expected_raw = expected.checked_mul(ten_pow_decimals()).ok_or_else(|| {
                    EasyTesterError::runtime("expected * 10^18 overflow".to_string())
                })?;

                if *balance != expected_raw {
                    return Err(EasyTesterError::runtime(format!(
                        "token balance mismatch: expected {} (raw {}), got raw {}",
                        expected, expected_raw, balance
                    )));
                }
                return Ok(format!(
                    "balance = {} (raw {} == {})",
                    expected, expected_raw, balance
                ));
            }
            return Err(EasyTesterError::runtime(format!(
                "unsupported expect path '{}' after read token_balance (supported: balance)",
                target.path
            )));
        }

        // Token metadata
        if let Some(metadata) = &self.last_token_metadata {
            if expected_value == "__not_empty__" {
                let val = metadata.get(&target.path).ok_or_else(|| {
                    EasyTesterError::runtime(format!(
                        "metadata field '{}' not found in response",
                        target.path
                    ))
                })?;
                if val.is_empty() {
                    return Err(EasyTesterError::runtime(format!(
                        "expected {}.not_empty, but value is empty",
                        target.path
                    )));
                }
                return Ok(format!("{}.not_empty (got '{}')", target.path, val));
            }

            let val = metadata.get(&target.path).ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "metadata field '{}' not found in response",
                    target.path
                ))
            })?;
            if val != expected_value {
                return Err(EasyTesterError::runtime(format!(
                    "metadata {} expected '{}', got '{}'",
                    target.path, expected_value, val
                )));
            }
            return Ok(format!("{} = '{}'", target.path, val));
        }

        Err(EasyTesterError::runtime(
            "no result available — preceding read was not supported or returned no data",
        ))
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn resolve_token_address(
        &mut self,
        token_name: &str,
        subnet_name: &str,
    ) -> Result<String, EasyTesterError> {
        let reg = self
            .registered_tokens
            .get(token_name)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!("token '{token_name}' not registered"))
            })?
            .clone();

        if subnet_name == reg.home_subnet {
            return Ok(reg.home_address.clone());
        }

        let key = (token_name.to_string(), subnet_name.to_string());
        if let Some(addr) = self.wrapped_addresses.get(&key) {
            return Ok(addr.clone());
        }

        self.query_and_cache_metadata(token_name, subnet_name)?;
        self.wrapped_addresses.get(&key).cloned().ok_or_else(|| {
            EasyTesterError::runtime(format!(
                "token '{}' has not been bridged to '{}' yet — no wrapped address found",
                token_name, subnet_name
            ))
        })
    }

    fn query_and_cache_metadata(
        &mut self,
        token_name: &str,
        subnet_name: &str,
    ) -> Result<(), EasyTesterError> {
        let metadata = self.query_metadata(token_name, subnet_name)?;
        if let Some(wrapped) = metadata.get("wrapped_token") {
            if !wrapped.is_empty()
                && wrapped != "0x0000000000000000000000000000000000000000"
                && wrapped.starts_with("0x")
            {
                self.wrapped_addresses.insert(
                    (token_name.to_string(), subnet_name.to_string()),
                    wrapped.clone(),
                );
            }
        }
        Ok(())
    }

    fn query_metadata(
        &self,
        token_name: &str,
        subnet_name: &str,
    ) -> Result<HashMap<String, String>, EasyTesterError> {
        let token_reg = self.registered_tokens.get(token_name).ok_or_else(|| {
            EasyTesterError::runtime(format!("token '{token_name}' not registered"))
        })?;

        let home_ds = self
            .discovered_subnets
            .get(&token_reg.home_subnet)
            .ok_or_else(|| {
                EasyTesterError::runtime(format!(
                    "home subnet '{}' not found",
                    token_reg.home_subnet
                ))
            })?;
        let query_ds = self
            .discovered_subnets
            .get(subnet_name)
            .ok_or_else(|| EasyTesterError::runtime(format!("subnet '{subnet_name}' not found")))?;

        let output = self.docker_exec_with_print(
            &[
                "ipc-cli",
                "--config-path",
                IPC_CLI_CONFIG,
                "cross-msg",
                "query-token-metadata",
                "--subnet",
                &query_ds.subnet_id,
                "--home-subnet",
                &home_ds.subnet_id,
                "--home-token",
                &token_reg.home_address,
            ],
        )?;

        parse_metadata_output(&output)
    }
}

// ── Free functions ─────────────────────────────────────────────────────

fn docker_exec(container: &str, args: &[&str]) -> Result<String, EasyTesterError> {
    debug!("docker exec {} {}", container, args.join(" "));
    let output = Command::new("docker")
        .arg("exec")
        .arg(container)
        .args(args)
        .output()
        .map_err(|e| EasyTesterError::runtime(format!("failed to run docker exec: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if !output.status.success() {
        return Err(EasyTesterError::runtime(format!(
            "docker exec failed (exit {}): {}\nstderr: {}",
            output.status.code().unwrap_or(-1),
            stdout.trim(),
            stderr.trim()
        )));
    }

    if !stderr.trim().is_empty() {
        debug!("docker exec stderr: {}", stderr.trim());
    }

    Ok(stdout)
}

fn parse_private_key_from_wallet_list(output: &str, address: &str) -> Result<String, String> {
    let addr_lower = address.to_lowercase();
    let mut found_address = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Address:") {
            let addr = trimmed.strip_prefix("Address:").unwrap().trim();
            found_address = addr.to_lowercase() == addr_lower;
        } else if found_address && trimmed.starts_with("Secret key:") {
            let sk = trimmed.strip_prefix("Secret key:").unwrap().trim();
            return Ok(sk.to_string());
        }
    }

    Err(format!("address {} not found in wallet list", address))
}

/// Parse `[[subnets]]` entries from config.toml where network_type = "fevm".
/// Returns (subnet_id, port, gateway_addr).
fn parse_fevm_subnets_from_config(toml_text: &str) -> Vec<(String, u16, String)> {
    let mut results = Vec::new();
    let mut current_id: Option<String> = None;
    let mut current_type: Option<String> = None;
    let mut current_provider: Option<String> = None;
    let mut current_gateway: Option<String> = None;

    let flush = |id: &Option<String>,
                 net_type: &Option<String>,
                 provider: &Option<String>,
                 gateway: &Option<String>,
                 results: &mut Vec<(String, u16, String)>| {
        if let (Some(id), Some(t), Some(p), Some(g)) = (id, net_type, provider, gateway) {
            if t == "fevm" {
                if let Ok(port) = extract_port(p) {
                    results.push((id.clone(), port, g.clone()));
                }
            }
        }
    };

    for line in toml_text.lines() {
        let trimmed = line.trim();

        if trimmed == "[[subnets]]" {
            flush(
                &current_id,
                &current_type,
                &current_provider,
                &current_gateway,
                &mut results,
            );
            current_id = None;
            current_type = None;
            current_provider = None;
            current_gateway = None;
            continue;
        }

        if let Some((key, val)) = trimmed.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"');
            match key {
                "id" => current_id = Some(val.to_string()),
                "network_type" => current_type = Some(val.to_string()),
                "provider_http" => current_provider = Some(val.to_string()),
                "gateway_addr" => current_gateway = Some(val.to_string()),
                _ => {}
            }
        }
    }

    // Flush last entry
    flush(
        &current_id,
        &current_type,
        &current_provider,
        &current_gateway,
        &mut results,
    );

    results
}

fn extract_port(url: &str) -> Result<u16, String> {
    // Parse port from URL like "http://localhost:8545/" or "http://host.docker.internal:8545/"
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
    let port_str = host_port
        .rsplit(':')
        .next()
        .ok_or_else(|| format!("no port in URL '{url}'"))?;
    port_str
        .parse::<u16>()
        .map_err(|e| format!("invalid port in URL '{url}': {e}"))
}

fn poll_block_advancing(docker: &str, rpc_url: &str) -> Result<(), EasyTesterError> {
    let deadline = Instant::now() + Duration::from_secs(60);

    let get_block = || -> Result<u64, EasyTesterError> {
        let output = docker_exec(docker, &["cast", "block-number", "--rpc-url", rpc_url])?;
        output
            .trim()
            .parse::<u64>()
            .map_err(|e| EasyTesterError::runtime(format!("failed to parse block number: {e}")))
    };

    let first = get_block()?;
    thread::sleep(Duration::from_secs(3));

    loop {
        let current = get_block()?;
        if current > first {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(EasyTesterError::runtime(format!(
                "blocks not advancing on {} (stuck at {})",
                rpc_url, first
            )));
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn parse_token_address_from_deploy_output(output: &str) -> Result<String, String> {
    // Look for "Deployed to: 0x..." or "Token deployed at: 0x..."
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Deployed to:") {
            let addr = rest.trim();
            if addr.starts_with("0x") {
                return Ok(addr.to_string());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("Token deployed at:") {
            let addr = rest.trim();
            if addr.starts_with("0x") {
                return Ok(addr.to_string());
            }
        }
    }
    Err("could not find 'Deployed to:' or 'Token deployed at:' in output".to_string())
}

fn parse_u256_from_cast_output(output: &str) -> Result<U256, String> {
    let s = output.trim();
    if s.is_empty() {
        return Err("empty output".to_string());
    }
    // cast may append annotations like " [1e24]" — strip them
    let s = s.split('[').next().unwrap_or(s).trim();
    // Handle hex output
    if s.starts_with("0x") || s.starts_with("0X") {
        return s
            .parse::<U256>()
            .map_err(|e| format!("hex parse error: {e}"));
    }
    // Decimal
    s.parse::<U256>()
        .map_err(|e| format!("decimal parse error: {e}"))
}

fn parse_metadata_output(output: &str) -> Result<HashMap<String, String>, EasyTesterError> {
    let mut metadata = HashMap::new();

    // Try JSON first
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(output.trim()) {
        if let Some(obj) = json.as_object() {
            for (k, v) in obj {
                if v.is_null() {
                    continue;
                }
                metadata.insert(k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string());
            }
            return Ok(metadata);
        }
    }

    // Fallback: key-value lines like "wrapped_token: 0x..." or "name: MyToken"
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some((key, val)) = trimmed.split_once(':') {
            let key = key.trim().to_lowercase().replace(' ', "_");
            let val = val.trim().to_string();
            if !val.is_empty() {
                metadata.insert(key, val);
            }
        }
    }

    if metadata.is_empty() {
        return Err(EasyTesterError::runtime(format!(
            "could not parse metadata from ipc-cli output:\n{output}"
        )));
    }

    Ok(metadata)
}
