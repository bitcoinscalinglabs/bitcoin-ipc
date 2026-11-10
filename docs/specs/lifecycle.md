# Subnet lifecycle

This document follows a subnet from creation to end: who submits which transaction and when, how the monitor derives the configurations and status from Bitcoin L1, the state it keeps and how it processes Bitcoin blocks. The last section gives the conditions on the parameter values. Slashing is in [slashing.md](slashing.md), the transaction formats and the monitor's checks in `transactions/`, the encodings and scripts in [encoding.md](encoding.md).

## Actors

- The **creator** chooses the subnet's parameters and submits the create transaction.
- A **validator** holds a validator key, an x-only key with even Y. Through its provider, it joins with a *collateral*, locked in its own collateral UTXO ([join](transactions/join.md)), signs checkpoints and slash transactions, submits checkpoints to Bitcoin L1 and exits after its collateral UTXO matures. It runs a subnet node.
- A **user** deposits BTC into a subnet, transfers within and between subnets, and withdraws to Bitcoin L1.
- The **subnet** is the validators of its current configuration running its consensus. It produces subnet blocks, decides the content of each checkpoint and collects the validators' signatures.
- A **provider** serves one validator or user. It builds, signs and submits their Bitcoin L1 transactions with its wallet and the validator key, and answers queries from its monitor's state.
- A **monitor** reads final Bitcoin L1 blocks and derives the state of every subnet from them alone. All honest monitors hold the same state.

[modules.md](modules.md) gives the programs that implement these roles.

## Parameters

### Network parameters

A Bitcoin L1 block is final once at least `CONFIRMATIONS` Bitcoin blocks are on top of it. A deployment sets `CONFIRMATIONS` once, for all its monitors. The prefix of every subnet id names the Bitcoin L1 network:

| Network | Prefix |
|---|---|
| mainnet | `/b1` |
| testnet3 | `/b2` |
| testnet4 | `/b22` |
| signet | `/b3` |
| regtest | `/b4` |

### Protocol constants

| Name | Value | Meaning |
|---|---|---|
| `MIN_CONFIGURATION_SIZE` | 4 | the number of validators below which a configuration may not make progress |
| `MAX_TOTAL_POWER` | 2³¹ − 1 | the maximum total power of a configuration |

### Subnet parameters

The creator sets every subnet parameter in the [create transaction](transactions/create-subnet.md). No parameter changes afterwards.

| Name | Meaning |
|---|---|
| `min_validator_stake` | the minimum value of the collateral UTXO of a join transaction, in satoshis |
| `power_unit` | the number of satoshis per unit of power, `10^p` for an integer `p` with `0 ≤ p ≤ 15` |
| `boot_validators` | the number of accepted join transactions that bootstraps the subnet (`min_validators` in the code). It also sets the threshold of `C_wlist` |
| `bottomup_check_period` | the number of subnet blocks between two checkpoints |
| `active_validators_limit` | the maximum number of validators of a configuration |
| `min_cross_msg_fee` | the minimum fee of a cross-subnet message, in satoshis |
| `whitelist` | the x-only keys that may join before activation |
| `K` | the length of an epoch, in Bitcoin L1 blocks |
| `min_timeout`, `max_timeout` | the minimum and maximum number of epochs for which a collateral UTXO seats its owner |
| `min_deposit` | the minimum value of a deposit or fee top-up transaction that the monitor credits, in satoshis (fee top-up transaction **unimplemented**) |
| `kill_vote_period` | the number of subnet blocks for which a kill proposal stays open |
| `kill_delay_period` | the number of epochs for which a subnet marked to be killed stays fully operational before its kill checkpoint |

[Instantiating the parameters](#instantiating-the-parameters) gives the conditions on their values.

## Power and thresholds

A validator whose seating collateral UTXOs hold `S` satoshis in total has power `⌊S / power_unit⌋`, on Bitcoin L1 (configuration multisigs) and on the subnet, which keeps each validator's total value. Every accepted collateral UTXO has power at least 1, because `min_validator_stake ≥ power_unit`. The monitor rejects a join or stake transaction that would bring the total power of the next configuration above `MAX_TOTAL_POWER`, so no configuration exceeds it.

A configuration of total power `P` has threshold `⌊2P / 3⌋ + 1`, except `C_wlist`, whose threshold is `⌊2 · boot_validators / 3⌋ + 1`.

## Configurations

A configuration (`SubnetCommittee` in the code) is a set of validators, each with a key and a power, with its own multisig address on Bitcoin L1. A subnet's configurations are `C_wlist` (the whitelist configuration), `C_gen` (the genesis configuration), then `C_1`, `C_2`, …, where `C_e` is the configuration of epoch `e`. The *current configuration* is the configuration of the current epoch as the monitor sees it. The *next configuration* is the one that the monitor computes at the next cutoff ([epochs](#epochs)). In epoch 0, `C_gen` runs the subnet and `C_wlist` holds the funds and signs the checkpoints, so `C_wlist` is the previous configuration of epoch 1.

## Lifecycle

### Creating the subnet

The creator submits the [create transaction](transactions/create-subnet.md). The monitor checks the [validity of the parameters](#validity-of-the-parameters), derives the [subnet id](encoding.md#subnet-id) from the txid of the reveal transaction (`SubnetId::from_txid` in the code) and stores the subnet's genesis info. The subnet is now *created*.

### Configuration C_wlist

`C_wlist` holds the keys of `whitelist`, each with power 1. Any member counts towards its threshold to spend `MS(C_wlist)`, whether or not it joined. `C_wlist` exists so that the genesis collateral UTXOs name a concrete configuration and their slash pre-signatures can be collected on a running subnet in epoch 0.

### Joins and prefunds before bootstrap

From creation until bootstrap, every validator with a whitelisted key may submit a [join transaction](transactions/join.md), whose collateral UTXO names `C_wlist`. Validators may also submit [prefund transactions](transactions/prefund.md), which pay `MS(C_wlist)` and give an EVM address a genesis balance on the subnet. Every member of `C_wlist` has a subnet account from genesis.

### Computing configuration C_gen

The subnet bootstraps at the `boot_validators`-th accepted join transaction, in the Bitcoin block at height `H_boot`. The monitor then computes `C_gen`, the genesis configuration: the validators of the first `boot_validators` accepted join transactions, each with power 1. The subnet is now *bootstrapped* and starts producing subnet blocks. Epoch 0 begins.

### Epoch 0

From `H_boot` on, the subnet produces subnet blocks with `C_gen` as its validator set. Epoch 0 prepares activation:

1. The members of `C_wlist` sign the slash transaction of the join collateral UTXO of every member of `C_gen`. The subnet collects these slash pre-signatures ([slashing](slashing.md#before-bootstrap-and-in-epoch-0)).
2. The monitor accepts no join transaction. Users submit deposit transactions and validators fee top-up transactions (fee top-up **unimplemented**) to `MS(C_wlist)`, and each is a fund UTXO of `C_wlist`. A validator needs a balance on its subnet account to submit its signatures ([join](transactions/join.md#creating-the-transaction)).
3. Epoch 0 has as many regular checkpoints of `C_wlist` as needed, one per checkpoint height, until `H_0` ([checkpoints of epoch 0](transactions/checkpoint.md#checkpoints-of-epoch-0)). Their batch-transfer transactions carry the slash pre-signatures of the collateral UTXOs accepted before activation. Those of the join collateral UTXOs of `C_gen` may take several of these checkpoints ([slash pre-signatures](slashing.md#slash-pre-signatures)). The next checkpoint is the rotation checkpoint of epoch 1.
4. The first *cutoff height* `H_0` is the first Bitcoin L1 height at which the join collateral UTXO of every member of `C_gen` is spent or [slashable](slashing.md#slashable-outputs). After the transactions of the Bitcoin block at `H_0`, the monitor sets `H_0`, computes `C_1` and activates the subnet. Epochs count from `H_0`.

![Bootstrap of a subnet, from the joins to activation](figures/bootstrap.svg)

*Bootstrap of a subnet, from the joins to activation: Bitcoin L1 blocks on top, subnet blocks below.*

### Activation

Activation ends epoch 0, when the monitor sets `H_0`. The subnet becomes *active*, and epoch 1 begins. The [subnet status](#subnet-status) table lists every status.

### Computing configuration C_1

The monitor computes `C_1` at `H_0` as at any cutoff ([C_e](#computing-configuration-c_e-for-e--2)). `C_1` holds the members of `C_gen` whose join collateral UTXO is unspent and [slashable](slashing.md#slashable-outputs) at `H_0`, with a timeout `T > H_0 + 3K`. A member's power is that of the total value of its join collateral UTXO and of its stake collateral UTXOs of epoch 0 that meet the same conditions. `C_1` may be smaller than `C_gen` and than `boot_validators`. If no member of `C_gen` qualifies, `C_1` is `C_gen`. If `C_1` has fewer than `MIN_CONFIGURATION_SIZE` members, the subnet may not make progress.

### After activation

From activation until the subnet is killed, new validators submit [join transactions](transactions/join.md), and validators that have joined add or renew collateral with [stake transactions](transactions/stake.md) (stake transaction **unimplemented**). A collateral UTXO stops seating its owner before it [matures](#maturity-of-a-collateral-utxo). The owner then spends it with the [exit transaction](transactions/exit.md), or leaves earlier with a [cooperative exit](#cooperative-exit). Users submit [deposit transactions](transactions/deposit.md), and validators supply the fee reserve with fee top-up transactions (fee reserve and fee top-up transactions **unimplemented**). The subnet creates [checkpoints](#checkpoints). Anyone who learns the EOTS secret key of a validator that equivocated submits the [slash transaction](transactions/slash.md) of its collateral UTXO. The validators may vote to [kill the subnet](#the-end-of-a-subnet).

### Epochs

After activation, the cutoff heights `H_e = H_0 + e · K`, for `e ≥ 1`, divide the subnet's life into epochs of `K` Bitcoin L1 blocks. Epoch `e ≥ 1` spans Bitcoin heights `(H_{e−1}, H_e]` and has one configuration, `C_e`. The Bitcoin block at `H_e` belongs to epoch `e`: the monitor checks its join transactions with `e` as the current epoch, then computes `C_{e+1}` ([processing Bitcoin L1 blocks](#processing-bitcoin-l1-blocks)).

Everything that the monitor accepts up to and including the Bitcoin block at `H_e` goes into `C_{e+1}`, which is then fixed. The join and stake transactions of Bitcoin blocks `H_e + 1` to `H_{e+1}` go into `C_{e+2}`. The monitor checks each join and stake transaction of epoch `e + 1` against the next configuration as filled so far: `C_{e+1}` with the changes accepted since `H_e` ([join](transactions/join.md#verifying-the-transaction)).

### Computing configuration C_e for e ≥ 2

When the monitor processes the cutoff block `H_{e−1}`, it computes `C_e`: every validator that holds, at that Bitcoin block, an unspent collateral UTXO that

1. is locked under the collateral script that names `C_{e−1}` (condition 1 **unimplemented**: the code seats a collateral UTXO whatever configuration its script names, because collateral refresh is not implemented),
2. has a timeout `T > H_{e+2}`, and
3. is slashable by the slash pre-signatures of checkpoints that the monitor has processed.

Each such validator has the power of the total value of those collateral UTXOs. If no validator qualifies, `C_e` is `C_{e−1}`. If `C_e` has fewer than `MIN_CONFIGURATION_SIZE` members, the subnet may not make progress.

From the cutoff on, `C_e` is the current configuration: new deposit and join transactions name it. The subnet's validator set becomes `C_e` once the subnet [applies its configuration updates](#applying-configuration-updates-in-the-subnet). `C_{e−1}` then finalizes no further subnet block.

### Checkpoints

Every `bottomup_check_period` subnet blocks, the subnet creates a [checkpoint](transactions/checkpoint.md). A *checkpoint height* is the height of a subnet block at which the subnet creates a checkpoint.

A checkpoint pays, from the subnet's fund UTXOs, the withdrawals and outgoing transfers that the subnet has recorded up to its subnet block, and carries slash pre-signatures. It also carries the configuration updates that the monitor has released up to that point. The monitor releases the [configuration updates](#configuration-updates) of `C_e` when the last Bitcoin block it has parsed is the cutoff `H_{e−1}`, which puts it in epoch `e`, and only then.

### Regular and rotation checkpoints

The **rotation checkpoint** of epoch `e ≥ 1` is the first checkpoint that the subnet creates after the monitor parses the cutoff block `H_{e−1}`. Each provider decides it from its own monitor and marks it with the flag bit `0x04` ([OP_RETURN data](transactions/checkpoint.md#op_return-data)). Every epoch has one, also when `C_e` equals `C_{e−1}`. When `MS(C_e)` differs from `MS(C_{e−1})`, the rotation checkpoint spends every fund UTXO under `MS(C_{e−1})` and pays its change to `MS(C_e)`. Otherwise it moves no funds. The rotation checkpoint of epoch 1 always spends the fund UTXOs under `MS(C_wlist)` and pays its change to `MS(C_1)`, also when `C_1` equals `C_gen`. The next checkpoints move the funds that do not fit. Every other checkpoint is a **regular checkpoint**, which pays its change to the multisig of its own configuration. A checkpoint belongs to the epoch of the latest rotation checkpoint at or before it, and to epoch 0 if there is none.

**Assumption.** A Bitcoin block at height at most `H_e` includes the rotation checkpoint of epoch `e`. The validators must recreate it until a Bitcoin block includes it. If none up to `H_e` does, the monitor still computes `C_{e+1}` at `H_e`, and the subnet's funds stay under the configuration that the last rotation checkpoint adopted until the next rotation checkpoint moves them to the configuration of its own epoch. The monitor also accepts a rotation checkpoint in a Bitcoin block above `H_e` ([verifying the checkpoint](transactions/checkpoint.md#verifying-the-transaction)).

### Configuration updates

The *configuration updates* of `C_e` (`StakeChangeRequest`) are the differences from `C_{e−1}` to `C_e` (from `C_gen` for `C_1`), validator by validator in ascending order of their keys:

- A validator that enters: a `Join` with its key, then a `Deposit` of the total value of its seating collateral UTXOs.
- A validator that stays and whose total value changes: a `Deposit` or a `Withdraw` of the difference.
- A validator that leaves: a `Withdraw` of its whole total value.

`Deposit` and `Withdraw` are the code's names for an increase and a decrease of a validator's collateral. They move no subnet funds.

The monitor numbers a subnet's configuration updates 1, 2, 3, …, across configurations. This number is the *change number* of a configuration update (`configuration_number` in the code, `next_configuration_number` in a checkpoint). The *last change number* of `C_e` is the change number of its last configuration update, or that of `C_{e−1}` if `C_e` has none. The last change number of `C_wlist` and of `C_gen` is 0.

<details>
<summary>**Example:**</summary>
The last change number of `C_2` is 7. At the cutoff `H_2`, one validator enters and one leaves. `C_3` has three configuration updates: a `Join` and a `Deposit` for the validator that enters, and a `Withdraw` for the one that leaves, with change numbers 8, 9 and 10. The last change number of `C_3` is 10. If nothing changes at `H_3`, `C_4` has no configuration update, and its last change number is also 10.
</details>

### Applying configuration updates in the subnet

The subnet's *parent finality* is the latest Bitcoin L1 height that the subnet has committed. With each commit, the subnet stores in its gateway the configuration updates up to that height. Every node reads them from its own monitor, through its provider, and must wait while its monitor has not processed that height or is unreachable. When the subnet creates a checkpoint at the end of subnet block `h`, it applies every configuration update that it has stored and not yet applied. Each part of the subnet switches from the previous configuration to the resulting one at its own subnet block:

- The consensus engine, which picks the proposer of each subnet block, and [EOTS votes](slashing.md#casting-an-eots-vote): from `h + 2`.
- The gateway and the validators' votes on the parent finality: from `h + 1`.
- Fendermint's lookup of the proposer of a subnet block: the previous and the resulting configuration, from `h + 1` until the subnet applies the next configuration updates.
- Checkpoint signing: the previous configuration signs the checkpoint of `h`, the resulting one the checkpoints from `h + 2` on.

### Collateral refresh

Every collateral UTXO must move once per epoch to the newest configuration, so that no seated collateral UTXO stays locked under a configuration whose validators have exited (collateral refresh **unimplemented**). A checkpoint moves a collateral UTXO with a *refresh pair*: an input that spends the collateral UTXO and the output at the same index that recreates it ([checkpoint format](transactions/checkpoint.md#transaction-format)).

- From the rotation checkpoint of epoch `e` on, checkpoints refresh each collateral UTXO locked under `C_{e−1}`, regular checkpoints included: the refresh pairs of many validators do not fit in one transaction, and an owner can sign only once `C_e` is known. The refresh pair recreates the collateral UTXO with the same value, keys and timeout and a slash leaf that names `C_e`. It needs the owner's signature and signatures of validators of `C_{e−1}` that reach its threshold.
- The validators must sign a refresh pair only for a collateral UTXO that matures after the end of epoch `e + 1`.
- A collateral UTXO that no checkpoint refreshes loses its seat in the next configuration, unless no validator enters, leaves or changes its power at `H_{e−1}`: its script then already names `C_e`.
- A validator of `C_{e−1}` must sign the refresh pairs and the checkpoint inputs locked under `C_{e−1}` until the end of epoch `e`, seated in `C_e` or not.

### Cooperative exit

A validator may leave before its collateral UTXO matures with a cooperative exit: a checkpoint spends the collateral UTXO through its cooperative-exit leaf and pays its value to an unbonding output of the same keys ([unbonding script](encoding.md#unbonding-script)), whose slash leaf names the checkpoint's configuration (cooperative exit **unimplemented**).

- The owner may sign at any time the exit of a collateral UTXO locked under the checkpoint's configuration or the previous one. Any checkpoint carries the signed exits.
- The validators must sign an exit only for a collateral UTXO that matures after the end of the epoch after the checkpoint's epoch, as for a refresh pair.
- The exited collateral UTXO no longer counts at later cutoffs. The unbonding output seats no one.
- The unbonding output stays slashable: the members of the checkpoint's configuration pre-sign its slash transaction, and the next checkpoint carries the slash pre-signatures ([slashing](slashing.md#after-activation)).
- From `3K` Bitcoin blocks after the Bitcoin block that includes the unbonding output, the owner spends it alone with the exit transaction.

### Maturity of a collateral UTXO

A collateral UTXO with timeout `T` seats its owner in `C_e` only if `T > H_{e+2}`. Its owner leaves at the first cutoff `H_{e−1}` with `T ≤ H_{e+2}`, unless another of its collateral UTXOs seats it. From Bitcoin block `T + 1` on, the owner spends the collateral UTXO with the exit transaction.

### The end of a subnet

A subnet ends when its validators kill it (kill procedure **unimplemented**). A member of the validator set submits a kill proposal on the subnet, which counts as its vote, and the other members vote. At most one proposal is open at a time, and a validator votes at most once per proposal. The subnet accepts the proposal if the current powers of its voters reach the threshold of their configuration within `kill_vote_period` subnet blocks. An expired proposal closes, and a new one may follow. The next checkpoint records the acceptance with the flag bit `0x02` ([OP_RETURN data](transactions/checkpoint.md#op_return-data)), and the monitor marks the subnet *to be killed*.

If that checkpoint belongs to epoch `e`, the subnet stays fully operational up to the end of epoch `e + kill_delay_period`, so that users withdraw their funds. The rotation checkpoint of epoch `e + kill_delay_period + 1` is the [kill checkpoint](transactions/checkpoint.md#the-kill-checkpoint): it exits the collateral UTXOs whose owners signed a cooperative exit and burns the rest of the subnet's funds. When the monitor processes it, it marks the subnet *killed*. From then on it accepts no checkpoint, join or stake transaction, computes no configuration, and every other subnet rejects transfers to it. It still processes the kill checkpoint's batch-transfer transaction. The exit and slash transactions stay open on Bitcoin L1.

## Subnet status

| Status | From the Bitcoin L1 block | Meaning |
|---|---|---|
| created | that includes the create transaction | joins and prefunds |
| bootstrapped | that includes the join transaction that completes `C_gen` (height `H_boot`) | [epoch 0](#epoch-0), no join transaction |
| active | at `H_0`, after its transactions (activation) | all operations, with `C_1`, `C_2`, … |
| to be killed | that includes the checkpoint that records an accepted kill proposal | operations continue until the kill checkpoint |
| killed | that includes the kill checkpoint | no further operation |

## State kept by the monitor

Per subnet, with the code's names in parentheses:

- **Genesis info** (`SubnetGenesisInfo`), from creation on: subnet id, parameters, `C_wlist`, the accepted join transactions before bootstrap, `H_boot` (`genesis_block_height`), and the genesis balance entries (address, amount, prefund txid, height).
- **Subnet state** (`SubnetState`), from bootstrap on (before it, a lookup returns "not found" and the genesis info answers): the current epoch, the epoch of the latest rotation checkpoint, the configuration it adopted (`committee`, `C_gen` before the first) and its last change number, the last checkpoint number, the fee reserve (**unimplemented**), the status, `H_0`, and the checkpoint number at which the subnet was marked to be killed.
- **Configurations** (`committee::<subnet>:<n>`): the configuration of every epoch.
- **Configuration updates** (`StakeChangeRequest`): per configuration from `C_1` on, each update with its kind, validator key and subnet address, amount, change number and the Bitcoin block at which the monitor released it.
- **Checkpoints** (`SubnetCheckpoint`): number, subnet block hash and height, Bitcoin height and txid, the txid and height of its batch-transfer transaction, the configuration that its change pays and its last change number, and the flags.
- **Fund UTXOs**: per configuration, the accepted outputs locked under its multisig.
- **Rootnet messages** (`RootnetMessage`): the top-down messages for the subnet, numbered by a per-subnet message nonce from 0 (funds from deposit transactions and incoming transfers, ERC20 registrations, ERC20 transfers), each with the Bitcoin block that produced it.
- **Validators**: per accepted join transaction, the validator key, EOTS key, backup address and subnet address.
- **Collateral UTXOs** (`UeCollateral`): per accepted join and stake transaction, the collateral outpoint, value, `T`, validator key, backup address, named configuration and txid. Per collateral refresh and cooperative exit, the new collateral UTXO or unbonding output with the replaced collateral UTXO's backup address and, for an unbonding output, its Bitcoin height. Per output, its spend.
- **Token balances**: per ERC20 token (home subnet, home token address) and subnet, the amount the subnet holds.
- **Slash pre-signatures**: per collateral UTXO and unbonding output, those of the entry that made it slashable.
- **EOTS nonce commitments**: per EOTS key, the [EOTS nonce commitments](encoding.md#eots-nonce-commitment) of its accepted join and stake transactions. A recorded EOTS key or EOTS nonce commitment never changes and is never removed.
- The height of the last processed Bitcoin block.

## Processing Bitcoin L1 blocks

- **Finality.** The monitor processes a Bitcoin block only once it is final, in height order without gaps. A reorganization deeper than `CONFIRMATIONS` Bitcoin blocks is outside this specification.
- **Order.** Transactions in their order in the Bitcoin block. Within a transaction, the messages in the input witnesses before the message in the outputs. After the transactions of a cutoff block, the next configuration.
- **Exact formats.** The monitor accepts a transaction only in the format this specification gives. The exception is a reveal transaction: it carries no signature, so anyone can rebuild it, and its other inputs, their order, its outputs and its fee are free. Each input that spends a commitment output must be exactly as specified, at any index, and the monitor checks each as a separate message, in input order. The reveal transaction of a [create transaction](transactions/create-subnet.md) has exactly one input and one output, because its txid gives the subnet id.
- **Atomicity.** The monitor applies a Bitcoin block as a whole or not at all (atomic Bitcoin block processing **unimplemented**): its state changes, a cutoff's configuration and the new last processed height take effect together. After a stop during a Bitcoin block, the monitor resumes from the previous state, and processing that Bitcoin block again gives the same result.
- **Determinism.** The state depends only on the final Bitcoin L1 blocks and the parameters, so every honest monitor of a deployment holds the same state at the same height. The monitor ignores a message that breaks its rules and never applies part of one. It records every spend of a fund UTXO, collateral UTXO or unbonding output that it holds, whatever spends it.

## Instantiating the parameters

### Validity of the parameters

When the monitor processes a create transaction, it checks:

- `p ≤ 15`, `min_validator_stake ≥ power_unit`, `0 < boot_validators`, `bottomup_check_period > 0`, `K > 0`, `min_cross_msg_fee > 0` and `kill_vote_period > 0`.
- The keys of `whitelist` are distinct, `boot_validators ≤ |whitelist| ≤ 350` and `boot_validators ≤ active_validators_limit ≤ 350`, so that every slash transaction stays within the 10,000 vB of TRUC ([slash](transactions/slash.md#transaction-format)).
- `1 ≤ min_timeout ≤ max_timeout` and `3K ≤ 65535`, the largest relative timelock in Bitcoin blocks.
- `min_deposit ≥ 330`, the dust limit of the P2TR output that a deposit transaction creates.

If a condition fails, the monitor ignores the transaction and creates no subnet.

### Assumptions

All bounds count Bitcoin L1 blocks.

- **Finality.** A Bitcoin block with at least `d = CONFIRMATIONS` Bitcoin blocks on top stays in the chain of every honest party. It and its transactions are final.
- **Inclusion.** Within `Δ_incl` Bitcoin blocks after an honest party submits a valid transaction with a sufficient fee, a Bitcoin block includes it or another transaction that spends one of its inputs.
- **Signing.** The subnet signs and submits a checkpoint within `Δ_sign` Bitcoin blocks after the latest Bitcoin block that it takes into account is final, including the wait for the previous checkpoint and the recreation of a checkpoint whose input another transaction spent.
- **Checkpoint period.** The subnet produces `bottomup_check_period` subnet blocks within `Δ_cpt` Bitcoin blocks.
- **Adoption.** A configuration stops finalizing subnet blocks within `Δ_adopt` Bitcoin blocks after the cutoff that ends its epoch is final.
- **Observation.** Within `Δ_obs` Bitcoin blocks after a validator signs two conflicting subnet blocks, every honest validator observes it, and one of them submits the slash transaction of each unspent collateral UTXO and unbonding output of that validator. Within `Δ_obs` Bitcoin blocks after a Bitcoin block includes a new collateral UTXO or unbonding output created from them, one of them submits its slash transaction.
- **Checkpoints per epoch.** In every epoch `e`, Bitcoin blocks up to `H_e` include the rotation checkpoint and, except in the epoch of the kill checkpoint, later checkpoints whose batch-transfer transactions carry the slash pre-signatures of the outputs that the rotation checkpoint created. Within the size of a standard transaction, the checkpoints of the epoch spend every fund UTXO of `C_{e−1}` (`C_wlist` in epoch 1), and the kill checkpoint every fund UTXO of the subnet.
- **Honest validators.** In every configuration, the validators that are not honest hold less than 1/3 of the power. Slashing of equivocation needs only that they hold at most 2/3.

### Choosing the parameter values

No monitor can check these conditions, because they relate the parameters to the bounds of the assumptions. The creator must choose `K` and `bottomup_check_period`, and the validators must keep the fee reserve (fee reserve **unimplemented**), so that these conditions hold:

- Anchoring, withdrawals and transfers are safe and live if `K/2 ≥ 2d + Δ_cpt + Δ_sign + Δ_incl` and the fee reserve pays the fees of every checkpoint and leaves a change output of at least `min_deposit`.
- Deposits are safe and live if `K/2 ≥ d + Δ_incl`, which the first condition implies.
- A validator that equivocates loses its collateral to the slash transaction if `K ≥ 2d + Δ_adopt + 2(Δ_obs + Δ_incl)`.

A reasonable choice is `boot_validators = |whitelist|`. The threshold of `C_wlist` is then above 2/3 of its members. With a smaller value it may not be: a `whitelist` of 10 keys with `boot_validators = 4` has threshold 3, so 3 non-honest validators, which the [honest validators](#assumptions) assumption allows, can spend `MS(C_wlist)`. The cost is that the subnet bootstraps only if every whitelisted validator joins.
