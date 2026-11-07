# Create subnet

The creator registers a new subnet, its parameters and its configuration `C_wlist`. Anyone can be the creator of a subnet.

## Transaction format

A [commit-reveal pair](../encoding.md#commit-reveal-pair) whose commit transaction has no outputs other than the commitment and the funder's change. The reveal transaction has exactly one input, which spends the commitment output, and exactly one output, which pays an address of the creator. A transaction of any other shape is not a create transaction. The monitor derives the subnet id from the txid of the reveal transaction ([subnet id](../encoding.md#subnet-id)).

Data `D` = `"IPCCRT"` ‖ postcard of:

| Field | Type | Meaning |
|---|---|---|
| `min_validator_stake` | amount | the minimum collateral of a join |
| `power_unit_exponent` | `u8` | `p`, with `power_unit = 10^p` sat and `p ≤ 15` |
| `boot_validators` | `u16` | the number of accepted joins that bootstraps the subnet, and sets the threshold of `C_wlist` |
| `bottomup_check_period` | `u64` | the number of subnet blocks between two checkpoints |
| `active_validators_limit` | `u16` | the maximum number of validators of a configuration |
| `min_cross_msg_fee` | amount | the minimum fee of a cross-subnet message |
| `whitelist` | list of x-only keys | the keys of `C_wlist` |
| `K` | `u64` | the length of an epoch, in Bitcoin L1 blocks |
| `min_timeout`, `max_timeout` | `u32`, `u32` | the minimum and the maximum number of epochs for which a collateral gives its owner a seat |
| `min_deposit` | amount | the minimum value of a deposit or fee top-up that the monitor credits (fee top-up **unimplemented**) |
| `kill_vote_period` | `u64` | the number of subnet blocks for which a kill proposal stays open |
| `kill_delay_period` | `u64` | the number of epochs for which a subnet marked to be killed stays fully operational before its kill checkpoint (the kill procedure, which uses `kill_vote_period` and `kill_delay_period`, **unimplemented**) |

The tag is the ASCII bytes `49 50 43 43 52 54`. Example (stake 1000 sat, `p` = 3, 2 validators, period 10, limit 20, fee 50, two keys, `K` = 144, timeouts 1 and 10, minimum deposit 330 sat, kill proposal period 36, kill delay 5): `49 50 43 43 52 54 e8 07 03 02 0a 14 32 02 ‖ key1 ‖ key2 ‖ 90 01 01 0a ca 02 24 05`, 86 bytes.

[Subnet parameters](../lifecycle.md#subnet-parameters) gives the meaning of every field, [validity of the parameters](../lifecycle.md#validity-of-the-parameters) the conditions the monitor checks, and [choosing the parameter values](../lifecycle.md#choosing-the-parameter-values) the conditions for safety and liveness. The specification gives no values.

## Creating the transaction

The creator must choose parameters that satisfy the validity conditions and the conditions for safety and liveness.

The creator's wallet funds and signs the commit transaction, and the creator submits the commit and the reveal transactions together. The provider's `createsubnet` call does this for its wallet and returns the subnet id.

## Verifying the transaction

1. A monitor finds the message in a reveal transaction with exactly one input and one output: the data of the leaf that the input executes starts with `IPCCRT` and decodes.
2. The monitor ignores the message unless the parameters satisfy the validity conditions.
3. The monitor derives the subnet id from the txid of the transaction that carries the message, and ignores the message if a subnet with that id exists. It computes `C_wlist` and stores the subnet's genesis info.

**Remark.** Two create transactions get the same subnet id only if the txids of their reveal transactions share the first 20 bytes, which is unlikely. The monitor then keeps the first subnet and ignores the second create transaction.

The message carries no signature, and the whitelist keys need not belong to the creator. If an observer replaces the reveal transaction with its own, the subnet gets another id.

**Blocking a create.** The reveal transaction must have exactly one input because the subnet id comes from its txid: two creates in one reveal transaction would get the same id. This lets anyone block a create. An observer who sees the reveal transaction rebuilds it with an extra input and gets that version mined first. The monitor then ignores the create, and the commitment output is spent. The creator loses the fees and must create the subnet again.

**Future work.** Derive the subnet id from the txid of the reveal transaction and the index of the input that carries the create. Each create in a reveal transaction then gets its own id, the reveal transaction needs no restriction on its inputs, and an extra input no longer blocks a create.
