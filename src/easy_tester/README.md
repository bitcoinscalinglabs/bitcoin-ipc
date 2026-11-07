# easy_tester

Scenario-based integration test runner for Bitcoin IPC.

```
cargo run --bin easy_tester -- --scenario <scenario_file> --tester <config_file>
```

Full-subnet scenarios (ipc-cli, fendermint, outages) run through `ue_scenario.sh`: see [ipc_tester/README.md](ipc_tester/README.md).

---

## Config file format

Space-separated `key value` lines (no `=`). Stored in `src/easy_tester/testers/`.

### Db / Monitor tester config

| Key | Values | Notes |
|-----|--------|-------|
| `tester` | `db` \| `monitor` | required |
| `activation_height` | integer | required for reward commands |
| `snapshot_length` | integer | required for reward commands |
| `ue_activation_height` | integer | unilateral exit (`join-ue`) active from this height; omitted = off |
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

A subnet's setup block takes `whitelist <validators...>`, `min <n>` (`boot_validators`) and
optionally `epoch_length <n>` (`K`, default 10). The other subnet parameters are fixed
(`build_create_subnet_msg` in `model.rs`: power unit `10^4` sat, timeouts of 1 to 1000 epochs,
minimum deposit 330 sat).

### Commands

| Command | Description |
|---------|-------------|
| `block <n>` | What follows goes into block `n`; all earlier blocks are mined. |
| `create <subnet> [@<shape>]` | Create a subnet (uses setup whitelist). With a shape the reveal is rebuilt (see "Reveal shapes"); the name still resolves to the subnet id the reveal names, so `read subnet_genesis` can check whether the monitor accepted it. |
| `checkpoint <subnet> [<subnet>...] [@<shape>]` | Submit a checkpoint message to Bitcoin (containing all queued join/stake/unstake and token-related requests for that subnet). Several subnets: one checkpoint each, their batch transfers in one reveal transaction; a shape rebuilds that reveal. |
| `join <subnet> <validator> <collateral_sats>` | Queue a join command. |
| `join-ue <subnet> <[subnet:]validator> <collateral_sats> <timeout_height> [<[subnet:]validator> <collateral_sats> <timeout_height>]... [@<shape>]` | Join with the collateral under the unilateral-exit script, maturing at `timeout_height`, which must satisfy rule 2.6 of the join (its epoch, counted from `H_0` or from the join's block before activation, is `e + 4` to `e + 1003`). Several triples: all the joins in one reveal transaction, in that order, each read from its own input; a triple's `subnet:validator` targets another subnet than the line's. A rejected join is logged, not fatal, so the scenario can check it. Needs `ue_activation_height`. |
| `stake <subnet> <validator> <amount_sats> [@raw]` | Queue a stake-increase command. `@raw` (monitor tester): the transaction is built with the library and the tester's wallet, not by the provider, so that the monitor's own rules decide; a refusal is logged, not fatal. The db tester's stakes are always hand-built. |
| `unstake <subnet> <validator> <amount_sats> [@raw]` | Queue a stake-decrease command; `@raw` as for `stake`. |
| `unstake-ue <subnet> <validator>` | The validator's provider spends its matured unilateral-exit collaterals to its backup address (`unstakeue`). A refusal is logged, not fatal. Monitor tester only. |
| `presign <subnet> <owner> by <signer> [<signer>...] [bad]` | Queues slash pre-signatures of the owner's latest collateral by the signers' keys for the subnet's next `checkpoint`, which carries them in its batch transfer. `bad` corrupts the first signature. The db tester hands the entry to the monitor as is; the monitor tester sends it through the provider's dev checkpoint route. |
| `register_token <subnet> <name> <symbol> <initial_supply>` | Queue an ERC20 token registration (ETR). Decimals are fixed at 18. |
| `mint_token <subnet> <token> <amount>` | Queue a supply increase (ETS). |
| `burn_token <subnet> <token> <amount>` | Queue a supply decrease (ETS). |
| `erc_transfer <src_subnet> <dst_subnet> <token> <amount>` | Queue a cross-subnet ERC20 transfer (ETX). |
| `wait <seconds>` | Pause execution for a fixed duration. |
| `read rootnet_msgs <subnet>` | Read rootnet messages for the subnet (from all Bitcoin blocks). |
| `read token_balance <subnet> <token>` | Read and cache the token balance on the subnet. |
| `read reward_results <snapshot>` | Read reward results for the given snapshot. |
| `read subnet_genesis <subnet>` | Fields: `exists`, and when it exists `bootstrapped`, `min_validators`, `genesis_validators`. |
| `read subnet_state <subnet>` | The bootstrapped subnet's state. Fields: `activated`, `epoch` (0 until activation), `h_0` (`none` until activation), `last_rotation_epoch`, `committee_number`, `pending_multisig_changes`. |
| `read validators <subnet> [current]` | The latest committee (the waiting one if any), or the current one. Fields: `count`, `names` (sorted, comma-separated), `waiting`, `<validator>.collateral`, `<validator>.ue`. |
| `read ue_collaterals <subnet>` | The unilateral-exit collateral records. Fields: `count`, `spent_count`, the records whose collateral outpoint a Bitcoin transaction has spent; `slashable_count`, the outputs the monitor recorded as slashable; per validator `<validator>.count`, and for the validator's record with the greatest timeout `<validator>.value`, `<validator>.timeout`, `<validator>.spent`, `<validator>.slashable`. |
| `read withdrawals <subnet>` | The Withdraw stake changes not yet confirmed by a checkpoint. Fields: `count`, `<validator>` (amount). |
| `expect result.<path> = <value>` | Assert a field in the **last** `read` result. |

### Reveal shapes

A reveal transaction carries no signature, so the monitor reads a message from any input that spends a commitment output. A trailing `@<shape>` on `create`, `join-ue` or `checkpoint` rebuilds the reveal before it is submitted (`bitcoin_utils::reshape_reveal`, the same function the unit tests use): `as-built` (default), `input-before` (a reveal that carries no message merged before it: the data input is the last), `input-after` (merged after it: the data input is the first), `inputs-around` (one before and one after: the data inputs are in the middle), `reversed` (the inputs in reverse order), `other-output` (every output pays another script), `extra-output` (output 0 split in two). The filler reveals spend an empty-data commitment, which the db tester fabricates and the monitor tester funds with its wallet.

The monitor tester reads `validators`, `ue_collaterals`, `withdrawals` and `subnet_genesis` straight from the monitor's database (read-only), the other reads through the provider. `block N` means "what follows goes into block N": all earlier blocks are mined, and a `read` at `N` sees the state after block `N - 1`. Unilateral-exit scenarios: `scenaria/test_ue_*.txt` with `testers/db-ue.txt` or `testers/monitor-ue.txt` (`test_ue_activation.txt` uses the `*-activation-110.txt` configs; `test_ue_exit.txt` runs on the monitor tester only).

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
- The checkpoints of a bootstrapped subnet before activation are regular checkpoints of epoch 0: built with `dev_gencheckpointpsbt`, signed by the whitelist members, spending the whitelist's fund UTXOs. Each `checkpoint` is a checkpoint of epoch 0, and its batch transfer carries the queued `presign` entries. The monitor sets `H_0` and activates the subnet at the first Bitcoin L1 height at which every unspent join collateral of a member of `C_gen` is slashable; the next `checkpoint` is the rotation checkpoint of epoch 1, spending to `C_1`.
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
