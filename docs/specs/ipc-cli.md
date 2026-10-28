# ipc-cli with a Bitcoin parent

`ipc-cli` commands for subnets whose parent is Bitcoin. For the Bitcoin side, `ipc-cli` calls the bitcoin-ipc provider's JSON-RPC.

## Configuration and subnet ids

- Config file: `--config-path <path>` or `IPC_CLI_CONFIG_PATH`, default `~/.ipc/config.toml`.
- The Bitcoin L1 parent is a `[[subnets]]` entry whose `id` is the network prefix alone, for example `/b4` ([network prefixes](lifecycle.md#network-parameters)), with `[subnets.config]` `network_type = "btc"`, `provider_http = "http://<host>:<PROVIDER_PORT>/api"`, optional `auth_token` (sent as `Authorization: Bearer`, equal to the provider's `PROVIDER_AUTH_TOKEN`) and optional `provider_timeout` in seconds.
- `--subnet` takes the child id, for example `/b4/t410f…`, the same string as the provider's subnet id. Commands that act on the parent find it as the parent of `--subnet`. Commands that act on the child, and the relayer, also need a `[[subnets]]` entry for the child id with `network_type = "fevm"`.
- `--network` (FVM address network) stays `testnet`, the default: the provider parses only `t`-prefixed FVM addresses.
- Keys: `join-ue` takes the validator key from the EVM keystore entry of `--from` (or the default entry), and the provider signs the join with its own key (`VALIDATOR_SK_PATH`). Both must be the same key. `unstake-ue` and `leave` sign with the provider's own key.
- Before bootstrap (until the join that completes the genesis validators), the provider has genesis info but no subnet state for the subnet. Every method that reads the subnet state answers `Subnet <id> not found.`, so `list-validators`, `kill`, `cross-msg fund`, `list-topdown-msgs`, `list-validator-changes` and `last-bottomup-checkpoint-height` fail. `getgenesisinfo` is the lookup. `create`, `join-ue` and `cross-msg pre-fund` work before bootstrap, and `pre-fund` works only then.
- Every call is `POST <provider_http>` with `{"jsonrpc":"2.0","id":1,"method":…,"params":{…}}`. Amounts are satoshis.

## Commands that call the provider

### Create a subnet

```
ipc-cli subnet create --parent <network prefix, e.g. /b4> --min-validators <n> --bottomup-check-period <blocks> [--active-validators-limit <n>] btc --min-validator-stake <sats> --power-unit-exponent <p> [--min-cross-msg-fee <sats>] --validator-whitelist <xonly>,<xonly>,… --epoch-length <blocks> --min-timeout <epochs> --max-timeout <epochs> --min-deposit <sats> --kill-vote-period <subnet blocks> --kill-delay-period <epochs>
```

- Provider: `createsubnet` with `min_validator_stake`, `min_validators`, `bottomup_check_period`, `active_validators_limit` (default 100), `min_cross_msg_fee` (default 1), `whitelist`, and `power_unit_exponent`, `epoch_length` (`K`), `min_timeout`, `max_timeout`, `min_deposit`, `kill_vote_period`, `kill_delay_period`, which have no default. Transaction: [create subnet](transactions/create-subnet.md).
- Output: logs `created subnet with id: <subnet id>`.

### Join

```
ipc-cli subnet join-ue [--from <addr>] --subnet <id> --collateral <sats> [--initial-balance <sats>] --ip <ip:port> --backup-address <bitcoin address> --epochs <n>
```

- Provider: optional `prefundsubnet`, then `joinsubnet` with the join fields and `epochs`, the number of epochs the collateral seats the validator ([join](transactions/join.md#creating-the-transaction) step 5). Transaction: [join](transactions/join.md).
- After joining: the validator's EVM address needs an account on the subnet before its fendermint can broadcast its signatures. A [prefund](#prefund-the-genesis-balance) before bootstrap (`--initial-balance`) or a [deposit](#deposit-into-the-subnet) after activation with `--to <validator EVM address>` creates it.
- Output: `join-ue transaction submitted at height: <n>`.

### Stake

```
ipc-cli subnet stake-ue [--from <addr>] --subnet <id> --collateral <sats> --epochs <n> [--renew <txid:vout>]
```

- Provider: `stakeue` with `subnet_id`, the collateral, `epochs` (as for the join) and, with `--renew`, the outpoint of a matured collateral of the validator to spend at input 0. Transaction: [stake](transactions/stake.md).
- Output: `stake transaction submitted at height: <n>`.

### Exit with the matured collateral

```
ipc-cli subnet unstake-ue --subnet <id>
```

- Provider: `unstakeue` with `subnet_id`. It also spends the unbonding outputs whose delay has passed. Transaction: [exit](transactions/exit.md).
- Output: the txid, the spent amount and the backup address.

### Refresh collaterals

```
ipc-cli subnet refresh --subnet <id> [--collateral <txid:vout>]
```

- The provider signs with its validator key and `SIGHASH_SINGLE|ANYONECANPAY` the refresh pair, for the subnet's current configuration, of the named collateral, or of every unspent collateral of its validator key in the subnet that is locked under the previous configuration. The signatures go to the subnet, and a later checkpoint of the epoch carries the refreshes ([collateral refresh](lifecycle.md#collateral-refresh)).
- Output: the collaterals whose refreshes were signed.

### Leave the subnet before the timeout

```
ipc-cli subnet leave --subnet <id> [--collateral <txid:vout>]
```

- The provider signs with its validator key and `SIGHASH_SINGLE|ANYONECANPAY` the exit pair, for the subnet's current configuration, of the named collateral, or of every unspent collateral of its validator key in the subnet. The signatures go to the subnet, and a later checkpoint carries the exits ([cooperative exit](lifecycle.md#cooperative-exit)).
- Output: the collaterals whose exits were signed.

### Vote to kill the subnet

```
ipc-cli subnet kill --subnet <id> btc
```

- Submits the validator's kill vote to the subnet, or a kill proposal if none is open, signed with the validator's key on the subnet ([the end of a subnet](lifecycle.md#the-end-of-a-subnet)). No Bitcoin L1 transaction.
- Output: none.

### List the validators

```
ipc-cli subnet list-validators --subnet <id>
```

- Provider: `getsubnet` with `subnet_id`.
- Output: one line `<f410 address>: <info>` per member of the current configuration (active) and per validator with an accepted join whom the current configuration does not seat yet (waiting). Before bootstrap: fails with `Subnet <id> not found.`

### Show the genesis epoch

```
ipc-cli subnet genesis-epoch --subnet <id>
```

- Provider: `getgenesisinfo` with `subnet_id`.
- Output: `genesis epoch: <Bitcoin height of the bootstrap>`.

### Deposit into the subnet

```
ipc-cli cross-msg fund --subnet <id> btc [--to <addr>] <sats>
```

- Provider: `fundsubnet` with `subnet_id`, `amount`, `address` (`--to`, else the default EVM key), then `getconfirmedcount`. Transaction: [deposit](transactions/deposit.md).
- Also creates the subnet account of a validator (`--to <validator EVM address>`), which it needs to broadcast signatures ([join](#join)).
- Output: `fund transaction submitted at height: <n>`.

### Top up the fee reserve

```
ipc-cli cross-msg fee-top-up --subnet <id> btc <sats>
```

- Provider: `fundsubnet` with `subnet_id` and `amount`, without `address`, then `getconfirmedcount`. Transaction: [fee top-up](transactions/deposit.md).
- Output: `fee top-up submitted at height: <n>`.

### Prefund the genesis balance

```
ipc-cli cross-msg pre-fund --subnet <id> btc [--to <addr>] <sats>
```

- Provider: `prefundsubnet` with `subnet_id`, `amount`, `address`. Transaction: [prefund](transactions/prefund.md).
- Output: logs `address pre-funded successfully`.

### List the top-down messages

```
ipc-cli cross-msg list-topdown-msgs --subnet <id> --from <height> --to <height>
```

- Provider: `getrootnetmessages` with `subnet_id`, `block_height` for each height, and `getblockhash` for a height without messages.
- Output: block hash and messages per height.

### List the validator changes

```
ipc-cli checkpoint list-validator-changes --subnet <id> --from-epoch <height> --to-epoch <height>
```

- Provider: `getstakechanges` with `subnet_id`, `block_height` for each height.
- Output: per height, the configuration changes the monitor recorded at a cutoff ([configuration numbers](lifecycle.md#adopting-a-new-configuration)).

### Show the last checkpoint height

```
ipc-cli checkpoint last-bottomup-checkpoint-height --subnet <id>
```

- Provider: `getsubnetcheckpoint` with `subnet_id`.
- Output: `height: <child height of the last checkpoint>` (0 without checkpoint).

### Run the checkpoint relayer

```
ipc-cli checkpoint relayer --subnet <id> [--checkpoint-interval-sec <s>] [--finalization-blocks <n>] [--submitter <addr>] [--max-parallelism <n>] [--metrics-address <ip:port>]
```

- Provider: each interval (default 15 s) `getsubnetcheckpoint`, `getgenesisinfo` (checkpoint period) and `finalizecheckpointpsbt` ([checkpoint](transactions/checkpoint.md)), the first checkpoint included. Defaults: `--finalization-blocks 0`, `--max-parallelism 4`.
- Output: runs until stopped.

Fendermint calls `gencheckpointpsbt`, `getstakechanges`, `getrootnetmessages`, `getgenesisinfo`, `getblockhash`, `getconfirmedcount` and, with the build feature `emission_chain` (validator rewards), `getrewardedcollaterals` through the provider's JSON-RPC. `getsubnet` (subnet state), `getsubnetbalance`, `gettokenbalance` and `getblockcount` are reached with direct JSON-RPC calls.

## Commands that run on the child subnet

These commands call the child subnet's EVM endpoint, not the provider.

### Withdraw to Bitcoin

```
ipc-cli cross-msg release [--from <addr>] --subnet <child id> btc --to <bitcoin address> <sats>
```

- Effect: a withdrawal on the child, which a later [checkpoint](transactions/checkpoint.md) pays.

### Transfer BTC to another subnet

```
ipc-cli cross-msg transfer [--source-gateway-address <addr>] --source-subnet <child id> [--source-address <addr>] --destination-subnet <child id> --destination-address <addr> <sats>
```

- Effect: a cross-subnet transfer on the source child, which a [batch transfer](transactions/batch-transfer.md) carries.

### Transfer an ERC20 token to another subnet

```
ipc-cli cross-msg transfer-erc [--source-gateway-address <addr>] --source-subnet <child id> [--source-address <addr>] --destination-subnet <child id> --destination-address <addr> --token <token address> <amount>
```

- Effect: an ERC20 transfer on the source child, which a [batch transfer](transactions/batch-transfer.md) carries.

### Show a wrapped token's metadata

```
ipc-cli cross-msg query-token-metadata --subnet <child id> --home-subnet <child id> --home-token <token address> [--gateway-address <addr>]
```

- Output: the token metadata stored on the queried child, as JSON.

### Send value within the subnet

```
ipc-cli subnet send-value [--from <addr>] --to <addr> --subnet <child id> <amount>
```


### Show the parent finality

```
ipc-cli cross-msg parent-finality --subnet <child id>
```

- Output: the latest Bitcoin height committed as parent finality in the child.

### List checkpoint signature bundles

```
ipc-cli checkpoint list-bottomup-bundle --subnet <child id> --from-epoch <height> --to-epoch <height>
```


### List quorum-reached events

```
ipc-cli checkpoint quorum-reached-events --subnet <child id> --from-epoch <height> --to-epoch <height>
```


### Show wallet balances

```
ipc-cli wallet balances --subnet <child id> --wallet-type <evm|btc|fvm>
```


The other `wallet` commands, the `util` commands and `config init` are local.
