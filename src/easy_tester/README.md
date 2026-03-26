# easy_tester

## RewardTester (minimal summary)

`RewardTester` is a test backend that *mocks the monitor observing IPC activity on Bitcoin* (the scenario commands) and applies the same DB mutations and validations the monitor would, by calling the same `ipc_lib` message APIs.

In addition, it constructs a `RewardTracker` from the scenario `config` using `RewardConfig::new()` and `RewardTracker::new_with_config()` and calls it at the same points as the monitor:

- After a block is mined/closed: `RewardTracker::update_after_block(db, height)`
- After a checkpoint command (once implemented): `RewardTracker::update_after_checkpoint(db, height, subnet_id, checkpoint)` and then later `update_after_block` when that block is mined/closed.

This `RewardTester` does **not** talk to Bitcoin RPC/watchonly RPC; it only operates on the local `HeedDb`.

## Scenario
- A block <N> line sets the current block height for subsequent commands.
- Commands (create, join, read, expect, …) execute at the current block height.
- The runner “mines” blocks when it needs to advance height (and it mines the final open block at EOF).

## Output
The `RewardTester` accepts commands like the following, which query the specified database using the specified keys and print the output.

## ErcTransferTester

`ErcTransferTester` tests the ERC20 token registration (ETR) and cross-subnet transfer (ETX) flow. Like `RewardTester`, it mocks the monitor by calling `ipc_lib` APIs directly against a local `HeedDb`.

It adds two scenario commands:

- `register_token <subnet> <name> <symbol> <decimals>` — queues an `IpcErcTokenRegistration` on the subnet. The registration is included in the next `checkpoint` for that subnet.
- `erc_transfer <src_subnet> <dst_subnet> <token_name> <amount>` — queues an `IpcCrossSubnetErcTransfer` for the named token. The token must have been previously registered with `register_token`. The transfer is included in the next `checkpoint` for the source subnet.

When `checkpoint <subnet>` runs, any pending registrations and transfers are embedded in the checkpoint message. The tester then simulates the batch transfer reveal step (`IpcBatchTransferMsg::save_to_db`), which fans out ETR records to all other subnets and saves ETX records to the destination subnet's `rootnet_msgs_db`.

### Reading and asserting rootnet messages

```
read rootnet_msgs <subnet>
```

Prints the rootnet messages stored for `<subnet>` and caches them for subsequent `expect` assertions.

All `expect` assertions use the unified `result.` prefix — the tester interprets the path based on what was last `read`:

```
expect result.count = <n>
expect result.<index>.<field> = <value>
```

Supported fields for rootnet messages:

| Field | Applies to | Value |
|-------|-----------|-------|
| `kind` | all | `fund`, `erc_transfer`, `erc_registration` |
| `amount` | fund, erc_transfer | decimal integer |
| `tokenName` | erc_registration | string |
| `tokenSymbol` | erc_registration | string |
| `tokenDecimals` | erc_registration | decimal integer |

### Config

```
config
tester ErcTransferTester
```

No additional parameters (unlike `RewardTester` which requires `activation_height` and `snapshot_length`).

## Usage

```
cargo run --bin easy_tester -- src/easy_tester/scenaria/test_rewards.txt
cargo run --bin easy_tester -- src/easy_tester/scenaria/test_erc_transfer.txt
```