# Checkpoint

A checkpoint is the Bitcoin L1 transaction that a subnet creates at each [checkpoint height](../lifecycle.md#checkpoints). It commits the subnet block, pays withdrawals and outgoing transfers from the subnet's fund UTXOs, refreshes and exits collaterals, and commits to the data that its [batch-transfer transaction](batch-transfer.md) reveals. A validator submits both together.

## Rotation and regular checkpoints

[Regular and rotation checkpoints](../lifecycle.md#regular-and-rotation-checkpoints) defines both kinds and the epoch to which a checkpoint belongs. The **checkpoint's configuration** is the configuration of its epoch (`C_wlist` in epoch 0), and its **previous configuration** that of the epoch before (`C_wlist` in epoch 1). Both kinds follow the format below, and the notes under each table say which rows each kind has.

## Transaction format

The checkpoint has version 2 and nLockTime 0, and every input has nSequence `0xffffffff`. In the tables, `p` is the number of collaterals that the checkpoint refreshes or exits, `W` the number of withdrawals it pays, and `B` the number of its transfer batches, one per destination subnet, each with any number of native transfers to that subnet. [Encoding](../encoding.md#notation) defines `r`, `fee_r`, `MS` and `txid20`.

### Inputs

| Index | Count | Input | Spent through | Signed by |
|---|---|---|---|---|
| 0 | 1 | the first fund input (list below) | leaf `M(C)` of `MS(C)`, with the [multisig witness](../encoding.md#configuration-multisig), where `C` is the configuration that locks the input | validators of `C` |
| 1 … p | p (note 1) | the collaterals that the checkpoint refreshes or exits | the [cooperative-exit leaf](../encoding.md#collateral-script) | the collateral's owner, and validators of the configuration that the collateral names |
| p + 1 … | the other fund inputs | the remaining fund inputs (list below) | as input 0 | as input 0 |

The fund inputs are fund UTXOs, in this order:

1. The change of the previous checkpoint (note 2).
2. Fund UTXOs of the earlier configurations that still hold funds, oldest configuration first (note 3).
3. Fund UTXOs of the checkpoint's configuration.

Within each group, and within each configuration of group 2, the inputs are in their order on Bitcoin L1, and each fund UTXO appears once. Within the size of a standard transaction, the provider takes the fund inputs first, then the pairs, then the payments, each as a prefix of its list. The next checkpoint spends the rest. The kill checkpoint spends every fund UTXO of the subnet. A checkpoint may spend the change output of the previous checkpoint before that checkpoint is in a final Bitcoin block. Every other input must spend an output of a transaction in a final Bitcoin block that the monitor has processed (**unimplemented**). No two checkpoints that are not final may spend the same fund UTXO.

The validators of the configuration that locks or names an input sign that input with `SIGHASH_DEFAULT`, whether or not they still hold a seat. Their signatures cover the whole transaction.

Notes:

1. Every checkpoint of epoch `e ≥ 1` refreshes the collaterals locked under `C_{e−1}` (`C_wlist` for `e = 1`) whose owners have signed the refresh pair and that no earlier checkpoint refreshed ([collateral refresh](../lifecycle.md#collateral-refresh)). It also exits the collaterals, locked under its configuration or the previous one, whose owners have signed a cooperative exit ([cooperative exit](../lifecycle.md#cooperative-exit)). `p` is 0 when there is none, and in epoch 0.
2. Present when the previous checkpoint has a change output. That change is locked under an earlier configuration in a rotation checkpoint, and under the checkpoint's configuration in a regular one. Checkpoint number 0 has no previous change.
3. In a rotation checkpoint whose configuration's multisig differs from that of the previous configuration, and in every rotation checkpoint of epoch 1, these are all fund UTXOs of the earlier configurations, which hold the subnet's funds when the epoch begins. In every other checkpoint, they are only those that arrived late or that an earlier checkpoint left out.

### Outputs

| Index | Count | Value | scriptPubKey |
|---|---|---|---|
| 0 | 1 | 0, or in the kill checkpoint the remainder, which this output burns | OP_RETURN with the 77 bytes of the checkpoint data |
| 1 … p | p (note 1) | the value of the collateral at the same input index | for a refresh pair, the [collateral script](../encoding.md#collateral-script) of that collateral's `pk`, `pk_eots` and `T`, and for a cooperative exit, the [unbonding script](../encoding.md#unbonding-script) of its `pk` and `pk_eots`. Both name the checkpoint's configuration |
| p + 1 … p + W | W | the withdrawal's amount | the withdrawal's Bitcoin L1 address |
| p + W + 1 | 1 if the checkpoint has batch-transfer data (a transfer, an ERC20 record or a slash pre-signature), otherwise 0 | `max(110·r, 330) + fee_r(w)`, where `w` is the weight of the batch-transfer transaction | the commitment to the batch-transfer data |
| next B | B, one per transfer batch, in ascending order of the destination's `txid20` | the sum of the transfers of the batch | `MS` of the destination subnet's current configuration. The monitor also accepts the multisig of an earlier configuration of the destination within its delayed-arrival window |
| last | 1, except 0 in the kill checkpoint | the remainder, at least `min_deposit` | change: `MS` of the checkpoint's configuration |

The remainder is the value of the inputs minus the other outputs and the fee. Every checkpoint except the kill checkpoint pays it as change. The kill checkpoint has no change: its output 0 burns the remainder.

Each collateral refresh or cooperative exit is a pair: input `i` and output `i`, for `1 ≤ i ≤ p`. The owner signs input `i` with `SIGHASH_SINGLE|ANYONECANPAY` (hash type `0x83`), which covers only input `i` and output `i`, so the owner's signature stays valid whatever else the checkpoint holds.

The same function must compute this output list for the provider that builds the checkpoint, the batch-transfer transaction that spends its commitment output, and the monitor that parses it.

### OP_RETURN data

Output 0 is an [OP_RETURN output](../encoding.md#op_return-output) whose data is these 77 bytes (this layout **unimplemented**: until the multisig-based join is removed, the code keeps a 78-byte layout with a count of that join's unstake outputs, a field this specification does not include):

| Offset (bytes) | Size (bytes) | Field | Encoding |
|---|---|---|---|
| 0 | 6 | tag | ASCII `IPCCPT` |
| 6 | 20 | subnet id | `txid20` |
| 26 | 32 | `checkpoint_hash` | the subnet block hash |
| 58 | 8 | `checkpoint_height` | `u64`, little-endian: the subnet block height |
| 66 | 1 | W | `u8`, the number of withdrawal outputs |
| 67 | 1 | B | `u8`, the number of transfer batches, one per destination subnet |
| 68 | 8 | `last_change_number` | `u64`, little-endian: the [change number](../lifecycle.md#configuration-updates) of the last configuration update that the subnet applied when it created the checkpoint, 0 if it applied none. A rotation checkpoint of epoch `e` may carry 0 or an older change number: the subnet applies the configuration updates of `C_e` once its parent finality reaches them, possibly in a later regular checkpoint of epoch `e` ([applying configuration updates in the subnet](../lifecycle.md#applying-configuration-updates-in-the-subnet)) |
| 76 | 1 | flags | bit `0x04` for a rotation checkpoint, bit `0x02` for the checkpoint that records an accepted kill proposal ([the end of a subnet](../lifecycle.md#the-end-of-a-subnet)) (bit `0x02` **unimplemented**, with the kill procedure), bit `0x01` for the kill checkpoint, every other bit 0 |

## Fund UTXOs

The fund UTXOs of a configuration are the outputs locked under its multisig that the monitor accepted: prefund transactions (for `C_wlist`), deposit and fee top-up transactions, incoming transfers that the monitor credited, and the change of the previous checkpoint.

The subnet accepts delayed arrivals for half an epoch (the `K/2` window **unimplemented**: the code accepts the multisig of the previous configuration instead). A deposit transaction, fee top-up transaction or incoming transfer that pays the multisig of the current configuration is a fund UTXO. One that pays the multisig of an earlier configuration `C_e` (`e ≥ 1`) or `C_wlist` is a fund UTXO if its block height is at most `H_e + K/2`, where `H_e` is the cutoff that ends epoch `e` (`H_0` for `C_wlist`) and `e` is the latest epoch whose configuration has that multisig ([epochs](../lifecycle.md#epochs)). The next checkpoint spends it into the current configuration.

The BTC of the fund UTXOs consists of the users' funds, which back the BTC minted on the subnet, and the **fee reserve** (fee reserve and fee top-up transactions **unimplemented**), which the validators supply through fee top-up transactions and which alone pays for checkpoints. The monitor keeps each subnet's fee reserve as a balance: every fee top-up transaction that it credits adds its value, and every checkpoint that it processes subtracts its cost. A checkpoint's **cost** is the value of its fund inputs minus the value of its withdrawal outputs, its transfer outputs, its change and, in the kill checkpoint, the value that output 0 burns. It equals the checkpoint's fee plus the value of its commitment output, which pays the batch-transfer transaction. A checkpoint's cost must be at most the fee reserve before it. The fund UTXOs also include output 0 of every processed batch-transfer transaction that pays `MS` of its checkpoint's configuration, which adds its value back to the fee reserve.

## Checkpoints of epoch 0

The checkpoints before the rotation checkpoint of epoch 1 are regular checkpoints with `C_wlist` as their configuration ([epoch 0](../lifecycle.md#epoch-0)):

- They spend fund UTXOs of `C_wlist`: prefund, deposit and fee top-up transactions (fee top-up **unimplemented**), and the change of the previous checkpoint. The validators of `C_wlist` sign them.
- They pay their change to `MS(C_wlist)`. They refresh and exit no collateral.

## The kill checkpoint

When the subnet is to be killed and the checkpoint that recorded the accepted kill proposal belongs to epoch `e`, the rotation checkpoint of epoch `e + kill_delay_period + 1` is the kill checkpoint ([the end of a subnet](../lifecycle.md#the-end-of-a-subnet)). Its flags have the bits `0x04` and `0x01`. It spends every fund UTXO of the subnet and exits the collaterals whose owners have signed a cooperative exit, as many as a standard transaction holds after the fund UTXOs. Its output 0 burns the remainder, so it has no change. The owner of any other collateral spends it through the unilateral-exit leaf once it matures. When the monitor processes the kill checkpoint, it marks the subnet killed, and the subnet creates no further checkpoint.

## Creating the transaction

1. Every validator's provider builds the checkpoint from the subnet's checkpoint data (subnet block hash and height, withdrawals, outgoing transfers, ERC20 records, `last_change_number`, the number `N` of slash pre-signatures that the subnet held at the end of the checkpoint's subnet block) and the monitor's state, and signs every input with its validator key once the checkpoint is final ([protocol consumers](../slashing.md#protocol-consumers)). It adds to the batch-transfer data one entry for every output that is not yet slashable and whose slash pre-signatures among the first `N` in the subnet reach the threshold, oldest first, as many as fit. A validator must sign the checkpoint only once the subnet holds such slash pre-signatures for every collateral and unbonding output that the checkpoint creates ([slashing](../slashing.md#after-activation)).
2. The checkpoint must satisfy every monitor rule below. Its pairs are in the order in which the subnet recorded the owners' signatures, and a collateral is in at most one pair: the first request recorded for it counts. It pays the longest prefix (**unimplemented**), in subnet order, of the withdrawals and then of the individual transfers that its inputs cover after the fee, within the fee reserve and within the size of a standard transaction for both the checkpoint and its batch-transfer transaction. The checkpoint groups the paid transfers into one batch per destination subnet, with the batch outputs sorted by `txid20`. A later checkpoint pays the payments it leaves out. The provider computes the fee for the largest witness, in which every key of each input's configuration signs. When the fee reserve cannot pay for a checkpoint, the subnet creates none until fee top-up transactions arrive (fee reserve and fee top-up transactions **unimplemented**). Every withdrawal and every transfer is at least the dust limit of its output.
3. The subnet collects the signatures per transaction. Once the signatures for every input reach the threshold of the configuration that locks it (threshold check before submission **unimplemented**), a validator assembles the witnesses and submits the transaction with its reveal transaction. For the kill checkpoint, it passes `maxburnamount` (in BTC) of at least the burned value.

Each provider uses its own [fee rate](../encoding.md#fee-rate), so providers can build different transactions for the same checkpoint. Only one of them collects enough signatures.

## Verifying the transaction

1. A monitor finds the checkpoint by output 0: an OP_RETURN with 77 data bytes starting with `IPCCPT`, naming a subnet.
2. A transaction that spends no fund UTXO of that subnet is not a checkpoint of the subnet. Every input must be a fund UTXO of the subnet or a collateral of the subnet that the monitor accepted (input check **unimplemented**). The inputs that spend collaterals must be inputs 1 … p, for some `p ≥ 0`, which gives `p`.
3. The outputs must follow the format above, with `p` from the inputs and `W` and `B` from the OP_RETURN data. The monitor checks the commitment output's value, which depends on the builder's fee rate, only against the dust limit of 330 sat. For `1 ≤ i ≤ p`, output `i` has the value of input `i` and, naming the checkpoint's configuration, the collateral script of the same keys and timeout for a refresh pair, or the unbonding script of the same keys for a cooperative exit. The checkpoint's cost is at most the fee reserve (fee reserve **unimplemented**). The monitor rejects a checkpoint that breaks any rule as a whole: no part of it takes effect.
4. Height order (**unimplemented**). The monitor records a checkpoint whose `checkpoint_height` is greater than that of the last recorded one, and ignores one with a lower height. It processes one with an equal height, with every state change of rule 6 and of its batch-transfer transaction, but gives it no checkpoint number. It keeps a record of that transaction (txid, flags, Bitcoin height), to which its batch-transfer transaction attaches, and the last recorded checkpoint stays.
5. Flags. A checkpoint with the bit `0x04` is the rotation checkpoint of the monitor's current epoch `e`, whatever its `last_change_number`. The monitor rejects it in epoch 0 and once it has recorded the rotation checkpoint of epoch `e`. The monitor gives that checkpoint and the later ones up to the next rotation checkpoint the configuration `C_e`. The bit `0x01` must be set exactly in the subnet's kill checkpoint. The bit `0x02` may be set only while the subnet is active. Every other bit is 0.
6. State changes. The monitor records the checkpoint (number = previous + 1, starting at 0) with its Bitcoin height, txid and `last_change_number`. It removes the spent fund UTXOs and adds the change as a fund UTXO of the configuration whose multisig it pays. It subtracts the checkpoint's cost from the fee reserve. It records each refreshed collateral under its new outpoint and each unbonding output that the checkpoint creates. It marks the subnet to be killed when it processes a checkpoint with the bit `0x02`, and killed when it processes the kill checkpoint.
7. The withdrawal outputs pay the subnet's withdrawals. The monitor cannot check them against the subnet's state and relies on the configurations that sign the checkpoint ([honest validators](../lifecycle.md#assumptions)). It credits the transfer outputs when it processes the batch-transfer transaction.

## Future work

Every checkpoint has at most one reveal transaction, and `boot_validators ≤ 90` keeps the slash pre-signatures of every member of `C_gen` within the reveal transaction of one checkpoint of epoch 0 ([slashing](../slashing.md#before-bootstrap-and-in-epoch-0)). Checkpoints may need to carry more data than one standard transaction holds, for example the slash pre-signatures of more genesis validators, or more transfers than one batch-transfer transaction holds, which otherwise wait for later checkpoints. The solution is several reveal transactions per checkpoint: the checkpoint carries one commitment output per reveal transaction, so it fixes all of them, and the monitor applies the checkpoint's data once it has processed every reveal transaction.
