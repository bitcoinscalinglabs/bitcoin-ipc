# easy_tester

Scenario-based integration test runner for Bitcoin IPC.

```
cargo run --bin easy_tester -- --scenario <scenario_file> --tester <config_file>
```

---

## Config file format

Space-separated `key value` lines (no `=`). Stored in `src/easy_tester/testers/`.

### Db / Monitor tester config

| Key | Values | Notes |
|-----|--------|-------|
| `tester` | `db` \| `monitor` | required |
| `activation_height` | integer | required for reward commands |
| `snapshot_length` | integer | required for reward commands |
| `monitor_log_level` | e.g. `debug`, `info` | monitor tester only; defaults to `info` |
| `provider_log_level` | e.g. `debug`, `info` | monitor tester only; defaults to `info` |

For the monitor tester, logs from `bitcoind`, `monitor`, `provider`, and the RPC client are written to `/tmp/easy_tester/` (overwritten on each run) and are not printed to the terminal. The log file locations are printed at the end of the run.

### Fendermint tester config

| Key | Values | Notes |
|-----|--------|-------|
| `tester` | `fendermint` | required |
| `subnet1 <id> <eth_rpc> <provider_url>` | — | declare a subnet (repeat for each) |
| `docker_container` | container name | defaults to `bitcoin-ipc` |
| `print_ipc_queries` | `on` \| `off` | print all ipc-cli / cast commands before execution |

Issuers (actors with EVM addresses) are declared in the scenario file's `setup` section, not in the config.

---

## Scenario file format (Db / Monitor tester)

A block line sets the current height; all commands that follow run at that height.

```
block 10
create subnet_a
join subnet_a validator1 100000

block 20
checkpoint subnet_a

read rootnet_msgs subnet_a
expect result.count = 1
expect result.0.kind = erc_registration
```

### Setup section

```
setup
validators validator1 validator2 validator3
subnet subnet_a min 2 whitelist validator1 validator2 validator3
```

### Commands

| Command | Description |
|---------|-------------|
| `block <n>` | Advance to block height `n`. Mines all intermediate blocks. |
| `create <subnet>` | Create a subnet (uses setup whitelist). |
| `checkpoint <subnet>` | Submit a checkpoint message to Bitcoin (containing all queued join/stake/unstake and token-related requests for that subnet). |
| `join <subnet> <validator> <collateral_sats>` | Queue a join command. |
| `stake <subnet> <validator> <amount_sats>` | Queue a stake-increase command. |
| `unstake <subnet> <validator> <amount_sats>` | Queue a stake-decrease command. |
| `register_token <subnet> <name> <symbol> <initial_supply>` | Queue an ERC20 token registration (ETR). Decimals are fixed at 18. |
| `mint_token <subnet> <token> <amount>` | Queue a supply increase (ETS). |
| `burn_token <subnet> <token> <amount>` | Queue a supply decrease (ETS). |
| `erc_transfer <src_subnet> <dst_subnet> <token> <amount>` | Queue a cross-subnet ERC20 transfer (ETX). |
| `wait <seconds>` | Pause execution for a fixed duration. |
| `read rootnet_msgs <subnet>` | Read rootnet messages for the subnet (from all Bitcoin blocks). |
| `read token_balance <subnet> <token>` | Read and cache the token balance on the subnet. |
| `read reward_results <snapshot>` | Read reward results for the given snapshot. |
| `expect result.<path> = <value>` | Assert a field in the **last** `read` result. |

---

## Scenario file format (Fendermint tester)

The fendermint tester uses a different scenario syntax. It runs against live fendermint subnets inside the Docker deployment.

### Setup section

Declares the tester type and the actors (issuers/users) with their EVM addresses:

```
setup
tester fendermint

issuer1 0x27b60d9f71d6806cca7d5a92b391093fe100f8e8
user1 0x005e05dd763dd125473f8889726f7c305e50fcae
user2 0xa78bc5d61e0da3c2d96e29a495f4e358d8d2218d

scenario
```

### Commands

| Command | Description |
|---------|-------------|
| `register_token <subnet> <issuer> <name> <symbol> <initial_supply>` | Deploy a BridgeableToken contract and register it on the gateway. Issuer receives the initial supply. |
| `deposit <subnet> <address_name> <amount_sats>` | Deposit native tokens (BTC) into a subnet for an address. |
| `erc_transfer <src_subnet> <src_actor> <dst_subnet> <dst_actor> <token> <amount>` | Cross-subnet ERC20 transfer. Amounts are in whole tokens (scaled by 10^18 internally). |
| `mint_token <subnet> <token> <amount>` | Mint additional tokens (issuer only). |
| `burn_token <subnet> <token> <amount>` | Burn tokens (uses `burnFrom`). |
| `wait <seconds>` | Pause execution for a fixed duration. |
| `read token_balance <subnet> <actor> <token>` | Read an actor's token balance on a subnet. |
| `read token_metadata <subnet> <token>` | Read token metadata (name, symbol, decimals, wrapped address). |
| `expect result.<field> = <value>` | Assert a field from the last `read`. Retries automatically (polls every 5s, up to 90s) to allow for cross-subnet settlement. |

Unsupported commands (`block`, `checkpoint`, `create`, `join`, `stake`, `unstake`) are rejected at parse time.

---

## Testers

### `DbTester` (`tester db`)

Mocks the monitor in-process by calling the same `ipc_lib` message APIs directly against a local `HeedDb` (in a temp dir). No Bitcoin RPC, no real network.

- All commands are supported.
- `activation_height` / `snapshot_length` enable reward tracking; reward commands fail at runtime if omitted.

### `MonitorTester` (`tester monitor`)

It tests the Bitcoin Monitor -- what it parses from Bitcoin and the state it maintains.
It runs a real integration stack: spawns `bitcoind`, compiles and starts `monitor` + `provider` (with `--release --features emission_chain,dev`), and drives them through their JSON-RPC APIs.

- Requires `bitcoind` and `bitcoin-cli` in PATH.
- On startup, creates a wallet and mines 101 blocks to mature coinbase UTXOs. **Scenario block heights must therefore start at 102 or higher**.
- After each block is confirmed, the tester automatically performs the **bootstrap handover** (`genbootstraphandover` → `dev_multisignpsbt` → `finalizebootstraphandover`) for any subnet that just reached its `min_validators` count. This moves collateral from the whitelist multisig address to the committee multisig address, which is required before the first checkpoint can be created. Scenarios do not need a dedicated command for this.
- Checkpoints use `dev_gencheckpointpsbt` (no balance check) → `dev_multisignpsbt` (all validator keys) → `finalizecheckpointpsbt`. This intentionally bypasses the provider's firewall so that the monitor's own enforcement is what is tested.
- Monitor and provider stderr are forwarded to the tester's stderr, prefixed with `[monitor]` / `[provider]`.
- The tester uses the provider compiled with the `--dev` flag for creating the required bitcoin transactions. For checkpoints, it uses `dev_gencheckpointpsbt()` in order to create wrong checkpoint messages, that a correct provider would never create (see `do_checkpoint_malicious()` in the tester code).
- Provider RPC errors surface the JSON-RPC `error.message` verbatim.
- All processes and the temp dir are cleaned up on exit.

**Read command support:**

| `read` command | Supported | Notes |
|---|---|---|
| `rootnet_msgs` | yes | `getrootnetmessages` |
| `token_balance` | yes | `gettokenbalance` |
| `subnet` | yes | `getsubnet` (print only) |
| `subnet_genesis` | yes | `getgenesisinfo` (print only) |
| `stake_changes` | yes | `getstakechanges` (print only; arg is block height) |
| `kill_requests` | yes | `getkillrequests` (print only; provider uses current height internally) |
| `reward_results` | yes | `getrewardedcollaterals`; supports `expect` |
| `committee` | no | no provider endpoint — use `DbTester` |
| `reward_candidates` | no | no provider endpoint — use `DbTester` |

### `FendermintTester` (`tester fendermint`)

Tests against live fendermint subnets running inside the Docker deployment. Deploys real ERC20 tokens, performs cross-subnet transfers via `ipc-cli`, and reads balances via `cast call`.

- Requires the Docker container (`bitcoin-ipc`) to be running with subnets spun up and relayers active.
- Issuer private keys are discovered from the `ipc-cli wallet list` output at startup.
- Gateway addresses are discovered from the validator's `config.toml`.
- Subnet liveness is verified (blocks advancing) before the scenario starts.
- `expect` commands automatically retry the preceding `read` (polling every 5s, up to 90s) to wait for cross-subnet settlement.

---

## All tests

### Db and Monitor tests

```bash
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_transfer.txt --tester src/easy_tester/testers/db.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_transfer.txt --tester src/easy_tester/testers/monitor.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_balances.txt --tester src/easy_tester/testers/db.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_balances.txt --tester src/easy_tester/testers/monitor.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_firewall.txt --tester src/easy_tester/testers/db.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_firewall.txt --tester src/easy_tester/testers/monitor.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_mint_burn.txt --tester src/easy_tester/testers/db.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_mint_burn.txt --tester src/easy_tester/testers/monitor.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_rewards.txt --tester src/easy_tester/testers/db.txt &&
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_rewards.txt --tester src/easy_tester/testers/monitor.txt
```

### Fendermint tests

```bash
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_fendermint_transfers.txt --tester src/easy_tester/testers/fendermint.txt
```
