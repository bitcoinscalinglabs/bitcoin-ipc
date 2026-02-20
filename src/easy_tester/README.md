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

## Usage

```
cargo run --features emission_chain --bin easy_tester -- src/easy_tester/scenaria/test_rewards.txt
```