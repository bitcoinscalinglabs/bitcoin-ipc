# easy_tester

Scenario-based integration test runner for Bitcoin IPC.

```
cargo run --bin easy_tester -- --scenario <scenario_file> --tester <config_file>
```

---

## Existing tests

Several scenarios are included under `src/easy_tester/scenaria/`. Example run against the db tester:

```bash
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_transfer.txt --tester src/easy_tester/testers/db.txt
```

Example run against the monitor tester (requires `bitcoind` in PATH):

```bash
cargo run --bin easy_tester -- --scenario src/easy_tester/scenaria/test_erc_transfer.txt --tester src/easy_tester/testers/monitor.txt
```
---

## Config file format

Space-separated `key value` lines (no `=`). Stored in `src/easy_tester/testers/`.

| Key | Values | Notes |
|-----|--------|-------|
| `tester` | `db` \| `monitor` | required |
| `activation_height` | integer | required for reward commands |
| `snapshot_length` | integer | required for reward commands |
| `monitor_log_level` | e.g. `debug`, `info` | monitor tester only; defaults to `info` |
| `provider_log_level` | e.g. `debug`, `info` | monitor tester only; defaults to `info` |

For the monitor tester, logs from `bitcoind`, `monitor`, `provider`, and the RPC client are written to `/tmp/easy_tester/` (overwritten on each run) and are not printed to the terminal. The log file locations are printed at the end of the run.

---

## Scenario file format

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
| `read rootnet_msgs <subnet>` | Read rootnet messages for the subnet (from all Bitcoin blocks). |
| `read token_balance <subnet> <token>` | Read and cache the token balance on the subnet. |
| `read reward_results <snapshot>` | Read reward results for the given snapshot. |
| `expect result.<path> = <value>` | Assert a field in the **last** `read` result. |

### Setup section (top of scenario file)

```
setup
validators validator1 validator2 validator3
subnet subnet_a min 2 whitelist validator1 validator2 validator3
```

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
