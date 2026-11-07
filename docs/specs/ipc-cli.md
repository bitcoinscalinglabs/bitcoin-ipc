# ipc-cli with a Bitcoin parent

The `ipc-cli` commands for subnets whose parent is Bitcoin L1. Commands that act on Bitcoin L1 or read the monitor's state go through the provider of the parent's entry in the configuration. The other commands go to the subnet's EVM endpoint.

## Common behaviour

- **Configuration.** `ipc-cli` reads `~/.ipc/config.toml`, or the file given with `--config-path`. The entry of the parent subnet names its provider (URL and bearer token).
- **Default key.** An optional `--from` or `--to` defaults to the default key of the EVM keystore.
- **Bitcoin L1 transactions.** The provider funds and signs them with its wallet, at the fee rate of its Bitcoin L1 node's estimate. The provider of a validator signs with that validator's key.
- **Heights.** "submitted at height `<n>`" gives the last block that the monitor has processed when the transaction is submitted, not the block that includes it.
- **Output.** Lines are written to stdout. A line marked "logs" is a log line, with timestamp and level, shown at log level `info`.
- **Errors.** Any error ends the command with exit code 1 and `main process failed: <error>`. A subnet missing from the configuration gives `subnet not found: <id>; known subnets: [<ids>]`. An error of the provider gives `JSON-RPC error: code=<code>, message=<message>, details=<details>`.

## Commands that act on Bitcoin L1

### Create a subnet

```
ipc-cli subnet create --parent <prefix> --min-validators <n> --bottomup-check-period <n> [--active-validators-limit <n>] btc --min-validator-stake <sats> --power-unit-exponent <p> [--min-cross-msg-fee <sats>] --validator-whitelist <keys> --epoch-length <n> --min-timeout <n> --max-timeout <n> --min-deposit <sats> --kill-vote-period <n> --kill-delay-period <n>
```

- Parameters: `--parent` is the prefix of the Bitcoin L1 network ([network parameters](lifecycle.md#network-parameters)). `--validator-whitelist` is a comma-separated list of x-only keys in hex. `--epoch-length` sets `K`. Every other flag sets the [subnet parameter](lifecycle.md#subnet-parameters) of the same name. Defaults: `--active-validators-limit` 100, `--min-cross-msg-fee` 1.
- Action: submits a [create transaction](transactions/create-subnet.md). Its reveal transaction pays a new address of the provider's wallet.
- Returns: logs `created subnet with id: <subnet id>`.

### Join

```
ipc-cli subnet join-ue [--from <address>] --subnet <id> --collateral <sats> [--initial-balance <sats>] --ip <ip:port> --backup-address <address> --epochs <n>
```

- Parameters: `--from` is the validator's EVM key. `--ip` is the validator's network address. `--backup-address` is the Bitcoin L1 address to which the exit transaction pays the collateral. `--epochs` is the number of epochs for which the collateral seats the validator.
- Action: with `--initial-balance` above 0, first submits a prefund transaction of that amount to the address of `--from` and logs `pre-funding address with <sats>`. Then submits a [join transaction](transactions/join.md). Its key `pk` is the x-only key of `--from`, which must be the provider's validator key. The provider sets the timeout `T` from `--epochs` and names the current configuration.
- Returns: `join-ue transaction submitted at height: <n>`.

### Stake

```
ipc-cli subnet stake-ue [--from <address>] --subnet <id> --collateral <sats> --epochs <n> [--renew <txid:vout>]
```

- Parameters: `--epochs` as for the join. `--renew` is a matured collateral of the validator that the stake transaction spends.
- Action: submits a [stake transaction](transactions/stake.md) (command **unimplemented**).
- Returns: `stake transaction submitted at height: <n>`.

### Exit with the matured collateral

```
ipc-cli subnet unstake-ue --subnet <id>
```

- Action: submits an [exit transaction](transactions/exit.md) that spends the validator's matured collaterals and the unbonding outputs whose delay has passed.
- Returns: `unstake-ue transaction submitted: txid <txid>; <amount> sats of collateral spent, paid minus the fee to <address>`.

### Refresh collaterals

```
ipc-cli subnet refresh --subnet <id> [--collateral <txid:vout>]
```

- Action: signs with the validator key and `SIGHASH_SINGLE|ANYONECANPAY` the refresh pair, for the subnet's current configuration, of the given collateral, or of every unspent collateral of the validator in the subnet that is locked under the previous configuration. The signatures go to the subnet, and a later checkpoint of the epoch carries the refresh pairs ([collateral refresh](lifecycle.md#collateral-refresh)) (command **unimplemented**).
- Returns: the collaterals whose refresh pairs were signed.

### Leave the subnet before the timeout

```
ipc-cli subnet leave --subnet <id> [--collateral <txid:vout>]
```

- Action: signs with the validator key and `SIGHASH_SINGLE|ANYONECANPAY` the cooperative-exit pair, for the subnet's current configuration, of the given collateral, or of every unspent collateral of the validator in the subnet. The signatures go to the subnet, and a later checkpoint carries the exits ([cooperative exit](lifecycle.md#cooperative-exit)) (command **unimplemented**).
- Returns: the collaterals whose exits were signed.

### Vote to kill the subnet

```
ipc-cli subnet kill --subnet <id> btc
```

- Action: posts the validator's kill vote on the subnet, or a kill proposal if none is open, signed with the validator key ([the end of a subnet](lifecycle.md#the-end-of-a-subnet)). No Bitcoin L1 transaction (vote on the subnet **unimplemented**).
- Returns: nothing.

### Deposit into the subnet

```
ipc-cli cross-msg fund --subnet <id> btc [--to <address>] <sats>
```

- Parameters: `--to` is the EVM address to credit.
- Action: submits a [deposit transaction](transactions/deposit.md) to the multisig of the subnet's current configuration.
- Returns: `fund transaction submitted at height: <n>`.

### Top up the fee reserve

```
ipc-cli cross-msg fee-top-up --subnet <id> btc <sats>
```

- Action: submits a fee top-up transaction ([deposit](transactions/deposit.md)) (command **unimplemented**).
- Returns: `fee top-up submitted at height: <n>`.

### Prefund the genesis balance

```
ipc-cli cross-msg pre-fund --subnet <id> btc [--to <address>] <sats>
```

- Parameters: `--to` is the EVM address that receives the genesis balance.
- Action: submits a [prefund transaction](transactions/prefund.md).
- Returns: logs `address pre-funded successfully`.

### Run the checkpoint relayer

```
ipc-cli checkpoint relayer --subnet <id> [--checkpoint-interval-sec <s>] [--finalization-blocks <n>] [--max-parallelism <n>] [--metrics-address <ip:port>]
```

- Parameters: defaults `--checkpoint-interval-sec` 15, `--finalization-blocks` 0, `--max-parallelism` 4. Metrics are served only with `--metrics-address`.
- Action: every interval, submits the next [checkpoint](transactions/checkpoint.md) with its batch-transfer transaction once its signatures reach the threshold. Every validator runs it.
- Returns: runs until stopped. Logs `checkpoint submitted on btc at height: <n>` for each checkpoint and `cannot submit checkpoint due to: <error>` on a failure.

## Queries of the monitor's state

### List the subnets

```
ipc-cli subnet list --parent <prefix>
```

- Returns: one line `<subnet id>: <status>, created at <Bitcoin height>` per subnet that the monitor has recorded, in the order of their create transactions. The status is one of the [subnet status](lifecycle.md#subnet-status) values. No subnet: no line (command **unimplemented**).

### List the validators

```
ipc-cli subnet list-validators --subnet <id>
```

- Returns: one line per member of the current configuration, then per validator with an accepted join transaction whom the current configuration does not seat yet: `<f410 address>: ValidatorInfo(staking: ValidatorStaking(confirmed_collateral: <BTC>, total_collateral: <BTC>, metadata: 0x), is_active: <bool>, is_waiting: <bool>)`. Amounts are in BTC. Before bootstrap it fails with `Subnet <id> not found.`

### Show the genesis epoch

```
ipc-cli subnet genesis-epoch --subnet <id>
```

- Returns: `genesis epoch: <H_boot>`. Before bootstrap it fails with `Invalid bootstrap_block_height`.

### List the top-down messages

```
ipc-cli cross-msg list-topdown-msgs --subnet <id> --from <height> --to <height>
```

- Parameters: `--from` and `--to` are Bitcoin L1 heights.
- Returns: per height, `block height: <height>, block hash: <hash>, number of messages: <n>`, then per message `from: <address>, to: <address>, message: <hex>, value: <amount>, nonce: <n>`. The value is in the subnet's smallest unit (10¹⁰ per sat).

### List the validator changes

```
ipc-cli checkpoint list-validator-changes --subnet <id> --from-epoch <height> --to-epoch <height>
```

- Parameters: `--from-epoch` and `--to-epoch` are Bitcoin L1 heights.
- Returns: per height, logs `changes at height: <height> are: <changes>`, the configuration updates that the monitor released at that Bitcoin block ([configuration updates](lifecycle.md#configuration-updates)).

### Show the last checkpoint height

```
ipc-cli checkpoint last-bottomup-checkpoint-height --subnet <id>
```

- Returns: `height: <n>`, the subnet height of the last recorded checkpoint, or 0 without a checkpoint.

## Commands that run on the subnet

These commands call the subnet's EVM endpoint.

### Withdraw to Bitcoin

```
ipc-cli cross-msg release [--from <address>] --subnet <id> btc --to <address> <sats>
```

- Parameters: `--to` is the Bitcoin L1 address to pay.
- Action: a withdrawal on the subnet, which a later [checkpoint](transactions/checkpoint.md) pays.
- Returns: `release performed in epoch: <subnet height>`.

### Transfer BTC to another subnet

```
ipc-cli cross-msg transfer [--source-gateway-address <address>] --source-subnet <id> [--source-address <address>] --destination-subnet <id> --destination-address <address> <sats>
```

- Parameters: `--source-gateway-address` defaults to the gateway of the configuration, `--source-address` to the default key.
- Action: a cross-subnet transfer on the source subnet, which a [batch-transfer transaction](transactions/batch-transfer.md) carries.
- Returns: `transfer performed in epoch: <subnet height>`.

### Transfer an ERC20 token to another subnet

```
ipc-cli cross-msg transfer-erc [--source-gateway-address <address>] --source-subnet <id> [--source-address <address>] --destination-subnet <id> --destination-address <address> --token <address> <amount>
```

- Parameters: `<amount>` is an integer in the token's smallest unit.
- Action: an ERC20 transfer on the source subnet, which a batch-transfer transaction carries.
- Returns: `transfer-erc performed in epoch: <subnet height>`. A revert gives `transfer-erc failed: <reason>`.

### Show a wrapped token's metadata

```
ipc-cli cross-msg query-token-metadata --subnet <id> --home-subnet <id> --home-token <address> [--gateway-address <address>]
```

- Returns: the JSON object `{"name", "symbol", "decimals", "wrapped_token"}` of the token on the queried subnet. `wrapped_token` is null on the home subnet. A token that is not registered gives `Token (<home subnet>, <home token>) is not registered on subnet <id>`.

### Send value within the subnet

```
ipc-cli subnet send-value [--from <address>] --to <address> --subnet <id> <amount>
```

- Parameters: `<amount>` is in BTC, a decimal number.
- Returns: logs `sending FIL from <from> to <to> in tx <hash>`.

### Show the parent finality

```
ipc-cli cross-msg parent-finality --subnet <id>
```

- Returns: the latest Bitcoin L1 height committed as parent finality on the subnet.

### List checkpoint signature bundles

```
ipc-cli checkpoint list-bottomup-bundle --subnet <id> --from-epoch <height> --to-epoch <height>
```

- Parameters: `--from-epoch` and `--to-epoch` are subnet heights.
- Returns: per height with a bundle, `bottom up checkpoint bundle at height: <height>`, then the bundle as one JSON line.

### List quorum-reached events

```
ipc-cli checkpoint quorum-reached-events --subnet <id> --from-epoch <height> --to-epoch <height>
```

- Parameters: `--from-epoch` and `--to-epoch` are subnet heights.
- Returns: one line per event, `QuorumReachedEvent<height: <height>, checkpoint: <hash>, quorum_weight: <weight>>`.

### Show wallet balances

```
ipc-cli wallet balances --subnet <id> --wallet-type <type>
```

- Parameters: `--wallet-type` is `evm`, `btc` or `fvm`.
- Returns: one line `<address> - Balance: <amount>` per key: for `evm` and `btc` the keys of the EVM keystore, with the balance in the subnet's smallest unit for `evm` and in sats for `btc`, and for `fvm` the keys of the FVM wallet. A failed lookup fails the whole command with `Error fetching balances`.

## Local commands

These commands need neither the provider nor the subnet.

### Generate the EOTS key

- Parameters: the paths of the two files to write.
- Action: generates a validator's EOTS key and its nonce seed ([EOTS key](slashing.md#eots-key-and-eots-nonce-commitments)), each a random 32-byte secret. It writes each to its file as 64 hex characters: the secret key of the EOTS key to the file that the validator's provider loads from `EOTS_SK_PATH`, and the nonce seed to the file that it loads from `EOTS_SEED_PATH` (command **unimplemented**).
- Returns: the EOTS key `pk_eots`, an x-only key, as 64 hex characters.

The other `wallet` commands, the `util` commands and `config init` are local.
