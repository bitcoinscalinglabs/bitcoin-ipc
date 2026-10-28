# Checkpoint

A checkpoint is the Bitcoin L1 transaction that a subnet creates every `bottomup_check_period` subnet blocks. It commits the subnet block, pays withdrawals and outgoing transfers from the subnet's fund UTXOs, refreshes and exits collaterals, and commits to the data that its [batch transfer](batch-transfer.md) reveals. A relayer submits both together.

## Rotation and regular checkpoints

- The **rotation checkpoint** of epoch `e + 1`, for `e ≥ 1`, is the first checkpoint that the subnet creates after it learns the final cutoff block `h_e` ([epochs](../lifecycle.md#epochs)). The [first checkpoint](#the-first-checkpoint), which activates the subnet, is the rotation checkpoint of epoch 1. Rotation checkpoints set the flag bit `0x04` ([OP_RETURN data](#op_return-data)).
- A **regular checkpoint** is every other checkpoint.

A checkpoint belongs to the epoch of the latest rotation checkpoint at or before it. The **checkpoint's configuration** is the configuration of its epoch, and its **previous configuration** that of the epoch before. A rotation checkpoint carries the configuration number of its configuration. A regular checkpoint carries 0, except the subnet's second checkpoint, which carries that of `C_1` ([configuration numbers](../lifecycle.md#adopting-a-new-configuration)). Both kinds follow the format below, and the notes under each table say which rows each kind has. [Regular and rotation checkpoints](../lifecycle.md#regular-and-rotation-checkpoints) describes the role of each kind, and [the kill checkpoint](#the-kill-checkpoint) the subnet's last checkpoint.

## Transaction format

The checkpoint has version 2 and nLockTime 0, and every input has nSequence `0xffffffff`. In the tables, `p` is the number of collaterals that the checkpoint refreshes or exits, `W` the number of withdrawals it pays, and `B` the number of its transfer batches, one per destination subnet, each with any number of native transfers to that subnet. [Encoding](../encoding.md#notation) defines `r`, `fee_r`, `MS` and `txid20`.

### Inputs

| Index | Count | Input | Spent through | Signed by |
|---|---|---|---|---|
| 0 | 1 | the first fund input (list below) | leaf `M(C)` of `MS(C)`, with the [multisig witness](../encoding.md#configuration-multisig), where `C` is the configuration that locks the input | validators of `C` |
| 1 … p | p (1) | the collaterals that the checkpoint refreshes or exits | the [cooperative-exit leaf](../encoding.md#collateral-script) | the collateral's owner, and validators of the configuration that the collateral names |
| p + 1 … | the other fund inputs | the remaining fund inputs (list below) | as input 0 | as input 0 |

The fund inputs are [fund UTXOs](#fund-utxos), in this order:

1. The change of the previous checkpoint (2).
2. Fund UTXOs of the previous configuration (3).
3. Fund UTXOs of the checkpoint's configuration.

Within each group, the inputs are in their order on Bitcoin L1, and each fund UTXO appears once. Within the size of a standard transaction, the provider takes the fund inputs first, then the pairs, then the payments, each as a prefix of its list. The next checkpoint spends the rest. The kill checkpoint spends every fund UTXO of the subnet. A checkpoint spends only outputs of transactions in final blocks that the monitor has processed, so the previous checkpoint's change is a fund input only once that checkpoint is final. No two checkpoints that are not final spend the same fund UTXO.

The validators of the configuration that locks or names an input sign that input with `SIGHASH_DEFAULT`, whether or not they still hold a seat. Their signatures cover the whole transaction.

Notes:

1. Every checkpoint of epoch `e + 1` except the first checkpoint refreshes the collaterals locked under `C_e` whose owners signed a refresh and that no earlier checkpoint refreshed ([collateral refresh](../lifecycle.md#collateral-refresh)). It also exits the collaterals, locked under its configuration or the previous one, whose owners have signed an exit ([cooperative exit](../lifecycle.md#cooperative-exit)). `p` is 0 when there is none, and in the first checkpoint.
2. Present when the previous checkpoint has a change output. That change is locked under the previous configuration in a rotation checkpoint, and under the checkpoint's configuration in a regular one. The first checkpoint has no previous change.
3. In a rotation checkpoint, these are all fund UTXOs of the previous configuration, which holds the subnet's funds when the epoch begins. In a regular checkpoint, they are only those that arrived late ([delayed arrivals](#fund-utxos)) or that an earlier checkpoint left out.

### Outputs

| Index | Count | Value | scriptPubKey |
|---|---|---|---|
| 0 | 1 | 0, or in the kill checkpoint the remainder, which this output burns | OP_RETURN with the 77 bytes of the [checkpoint data](#op_return-data) |
| 1 … p | p (1) | the value of the collateral at the same input index | for a refresh, the [collateral script](../encoding.md#collateral-script) of that collateral's `pk`, `pk_eots` and `T`, and for an exit, the [unbonding script](../encoding.md#unbonding-script) of its `pk` and `pk_eots`. Both name the checkpoint's configuration |
| p + 1 … p + W | W | the withdrawal's amount | the withdrawal's Bitcoin L1 address |
| p + W + 1 | 1 if the checkpoint has [batch transfer](batch-transfer.md) data (a transfer, an ERC20 record, a slash pre-signature or `h_0`), otherwise 0 | `max(110·r, 330) + fee_r(w)`, where `w` is the weight of the batch transfer transaction | the commitment to the batch transfer data |
| next B | B, one per transfer batch, in ascending order of the destination's `txid20` | the sum of the transfers of the batch | `MS` of the destination subnet's current configuration. The monitor also accepts the multisig of an earlier configuration of the destination within its delayed-arrival window ([fund UTXOs](#fund-utxos)) |
| last | 1, except 0 in the kill checkpoint | the remainder, at least `min_deposit` | change: `MS` of the checkpoint's configuration |

The remainder is the value of the inputs minus the other outputs and the fee. Every checkpoint except the kill checkpoint pays it as change. The kill checkpoint has no change: its output 0 burns the remainder.

Each refresh or exit is a pair: input `i` and output `i`, for `1 ≤ i ≤ p`. The owner signs input `i` with `SIGHASH_SINGLE|ANYONECANPAY` (hash type `0x83`), which covers only input `i` and output `i`, so the owner's signature stays valid whatever else the checkpoint holds.

The same function must compute this output list for the provider that builds the checkpoint, the batch transfer that spends its commitment output, and the monitor that parses it.

### OP_RETURN data

Output 0 is an [OP_RETURN output](../encoding.md#op_return-output) whose data is these 77 bytes:

| Offset (bytes) | Size (bytes) | Field | Encoding |
|---|---|---|---|
| 0 | 6 | tag | ASCII `IPCCPT` |
| 6 | 20 | subnet id | `txid20` |
| 26 | 32 | `checkpoint_hash` | the subnet block hash |
| 58 | 8 | `checkpoint_height` | `u64`, little-endian: the subnet block height |
| 66 | 1 | W | `u8`, the number of withdrawal outputs |
| 67 | 1 | B | `u8`, the number of transfer batches, one per destination subnet |
| 68 | 8 | `next_configuration_number` | `u64`, little-endian: the configuration number of its configuration in a rotation checkpoint, that of `C_1` in the subnet's second checkpoint, and 0 in every other checkpoint |
| 76 | 1 | flags | bit `0x04` for a rotation checkpoint, bit `0x02` for the checkpoint that records an accepted kill proposal ([the end of a subnet](../lifecycle.md#the-end-of-a-subnet)), bit `0x01` for the kill checkpoint, every other bit 0 |

## Fund UTXOs

The fund UTXOs of a configuration are the outputs locked under its multisig that the monitor accepted: prefunds (for `C_0`), deposits and fee top-ups, incoming transfers that the monitor credited, and the change of the previous checkpoint.

The subnet accepts delayed arrivals for half an epoch. A deposit, fee top-up or incoming transfer that pays the multisig of the current configuration is a fund UTXO. One that pays the multisig of an earlier configuration `C_e` (`e ≥ 0`) is a fund UTXO if its block height is at most `h_e + K/2`, with `h_0 = H_0` for `C_0`, where `h_e` is the cutoff that ends epoch `e` and `e` is the latest epoch whose configuration has that multisig ([epochs](../lifecycle.md#epochs)). The next checkpoint spends it into the current configuration.

The BTC of the fund UTXOs consists of the users' funds, which back the BTC minted on the subnet, and the **fee reserve**, which the validators supply through fee top-ups and which alone pays for checkpoints. The monitor keeps each subnet's fee reserve as a balance: every fee top-up that it credits adds its value, and every checkpoint that it processes subtracts its cost. A checkpoint's **cost** is the value of its fund inputs minus the value of its withdrawal outputs, its transfer outputs, its change and, in the kill checkpoint, the value that output 0 burns. It equals the checkpoint's fee plus the value of its commitment output, which pays the batch transfer. A checkpoint's cost must be at most the fee reserve before it. The fund UTXOs also include output 0 of every processed batch transfer that pays `MS` of its checkpoint's configuration, which adds its value back to the fee reserve.

## The first checkpoint

The first checkpoint is the rotation checkpoint from `C_0` to `C_1` that activates the subnet. [Epoch 0 and the first checkpoint](../lifecycle.md#epoch-0-and-the-first-checkpoint) says when the subnet creates it, and [C_1](../lifecycle.md#computing-configuration-c_1) how the monitor computes `C_1`. It is the only checkpoint between bootstrap and activation, and its format differs from the others:

- It spends the fund UTXOs of `C_0` (prefunds, deposits and fee top-ups), which the validators of `C_0` sign.
- Its [batch transfer](batch-transfer.md) data carries `H_0` and the slash pre-signatures of the members of `C_gen`, so it always has a reveal.
- Its change pays `MS(C_1)`, and its `next_configuration_number` is 0.
- The monitor processes it together with its reveal, which is in the same block or a later one. When it processes the reveal, it checks `H_boot ≤ H_0 <` the height of the block that includes the first checkpoint, computes `C_1`, checks that the change pays `MS(C_1)` and applies both transactions. Until then, the first checkpoint has no effect.

## The kill checkpoint

When the subnet is to be killed and the checkpoint that recorded the accepted kill proposal belongs to epoch `e`, the rotation checkpoint of epoch `e + kill_delay_period + 1` is the kill checkpoint ([the end of a subnet](../lifecycle.md#the-end-of-a-subnet)). Its flags have the bits `0x04` and `0x01`. It spends every fund UTXO of the subnet and exits the collaterals whose owners have signed an exit, as many as a standard transaction holds after the fund UTXOs. Its output 0 burns the remainder, so it has no change. The owner of any other collateral spends it through the unilateral-exit leaf once it matures. When the monitor processes the kill checkpoint, it marks the subnet killed, and the subnet creates no further checkpoint.

## Creating the transaction

1. Every validator's provider builds the checkpoint from the subnet's checkpoint data (block hash and height, withdrawals, outgoing transfers, ERC20 records, next configuration number) and the monitor's state, and signs every input with its validator key. It adds to the batch transfer data one entry for every output that is not yet slashable and whose pre-signatures in the subnet reach the threshold, oldest first, as many as fit. A validator signs the checkpoint only once the subnet holds such pre-signatures for every collateral and unbonding output that the checkpoint creates ([slashing](../slashing.md#after-activation)).
2. The checkpoint must satisfy every monitor rule below. Its pairs are in the order in which the subnet recorded the owners' signatures, and a collateral is in at most one pair: the first request recorded for it counts. It pays the longest prefix, in subnet order, of the withdrawals and then of the individual transfers that its inputs cover after the fee, within the fee reserve and within the size of a standard transaction for both the checkpoint and its batch transfer. The checkpoint groups the paid transfers into one batch per destination subnet, with the batch outputs sorted by `txid20`. A later checkpoint pays the payments it leaves out. The provider computes the fee for the largest witness, in which every key of each input's configuration signs. When the fee reserve cannot pay for a checkpoint, the subnet creates none until fee top-ups arrive. Every withdrawal and every transfer is at least the dust limit of its output.
3. The subnet collects the signatures per transaction. Once the signatures for every input reach the threshold of the configuration that locks it, the relayer assembles the witnesses and submits the transaction with its reveal. For the kill checkpoint, it passes `maxburnamount` (in BTC) of at least the burned value.

Each provider uses its own [fee rate](../encoding.md#fee-rate), so providers can build different transactions for the same checkpoint. Only one of them collects enough signatures.

## Verifying the transaction

1. A monitor finds the checkpoint by output 0: an OP_RETURN with 77 data bytes starting with `IPCCPT`, naming a subnet.
2. A transaction that spends no fund UTXO of that subnet is not a checkpoint of the subnet. Every input must be a fund UTXO of the subnet or a collateral of the subnet that the monitor accepted. The inputs that spend collaterals must be inputs 1 … p, for some `p ≥ 0`, which gives `p`.
3. The outputs must follow the format above, with `p` from the inputs and `W` and `B` from the OP_RETURN data. The monitor checks the commitment output's value, which depends on the builder's fee rate, only against the dust limit of 330 sat. For `1 ≤ i ≤ p`, output `i` has the value of input `i` and, naming the checkpoint's configuration, the collateral script of the same keys and timeout for a refresh, or the unbonding script of the same keys for an exit. The checkpoint's cost is at most the fee reserve ([fund UTXOs](#fund-utxos)). The monitor rejects a checkpoint that breaks any rule as a whole: no part of it takes effect.
4. Height order. The monitor records a checkpoint whose `checkpoint_height` is greater than that of the last recorded one, and ignores one with a lower height. It processes one with an equal height, with every state change of rule 7 and of its batch transfer, but gives it no checkpoint number. It keeps a record of that transaction (txid, flags, Bitcoin height), to which its batch transfer attaches, and the last recorded checkpoint stays.
5. `next_configuration_number` must be the configuration number of the current configuration in a rotation checkpoint other than the first, that of `C_1` in the subnet's second checkpoint if it is not a rotation checkpoint, and 0 in every other checkpoint.
6. Flags. The bit `0x04` marks the rotation checkpoint of the current epoch `e + 1`. It must be set in the first checkpoint and, after it, only in the first checkpoint that sets it since the cutoff `h_e`. The monitor gives that checkpoint and the later ones of the epoch the configuration `C_{e+1}`. The bit `0x01` must be set exactly in the subnet's [kill checkpoint](#the-kill-checkpoint). The bit `0x02` may be set only in a checkpoint other than the first while the subnet is active. Every other bit is 0.
7. State changes. The monitor records the checkpoint (number = previous + 1, starting at 0) with its Bitcoin height, txid and `next_configuration_number`. It removes the spent fund UTXOs and adds the change as a fund UTXO of the configuration whose multisig it pays. It subtracts the checkpoint's cost from the fee reserve. It records each refreshed collateral under its new outpoint and each unbonding output that the checkpoint creates. It marks the subnet to be killed when it processes a checkpoint with the bit `0x02`, and killed when it processes the kill checkpoint.
8. The withdrawal outputs pay the subnet's withdrawals. The monitor cannot check them against the subnet's state and relies on the configurations that sign the checkpoint ([honest validators](../lifecycle.md#assumptions)). It credits the transfer outputs when it processes the [batch transfer](batch-transfer.md).

## Future work

Every checkpoint has at most one reveal, and `min_validators ≤ 90` keeps the data of the first checkpoint within it ([slashing](../slashing.md#before-bootstrap-and-in-epoch-0)). Checkpoints may need to carry more data than one standard transaction holds, for example the pre-signatures of more genesis validators, or more transfers than one batch transfer holds, which otherwise wait for later checkpoints. The solution is several reveal transactions per checkpoint: the checkpoint carries one commitment output per reveal, so it fixes all of them, and the monitor applies the checkpoint's data once it has processed every reveal.
