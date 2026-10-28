# Create subnet

The creator registers a new subnet, its parameters and its configuration `C_0`. Anyone can be the creator of a subnet.

## Transaction format

A [commit-reveal pair](../encoding.md#commit-reveal-pair) whose commit has no outputs other than the commitment and the funder's change. The reveal has exactly one input, which spends the commitment output, and exactly one output, which pays an address of the creator. A transaction of any other shape is not a create subnet transaction. The monitor derives the subnet id from the reveal's txid ([subnet id](../encoding.md#subnet-id)).

Data `D` = `"IPCCRT"` ‖ postcard of:

| Field | Type | Meaning |
|---|---|---|
| `min_validator_stake` | amount | the minimum collateral of a join |
| `power_unit_exponent` | `u8` | `p`, with `power_unit = 10^p` sat and `p ≤ 15` |
| `min_validators` | `u16` | the number of accepted joins that bootstraps the subnet, and sets the threshold of `C_0` |
| `bottomup_check_period` | `u64` | the number of subnet blocks between two checkpoints |
| `active_validators_limit` | `u16` | the maximum number of validators of a configuration |
| `min_cross_msg_fee` | amount | the minimum fee of a cross-subnet message |
| `whitelist` | list of x-only keys | the keys of `C_0` |
| `K` | `u64` | the length of an epoch, in Bitcoin L1 blocks |
| `min_timeout`, `max_timeout` | `u32`, `u32` | the minimum and the maximum number of epochs for which a collateral gives its owner a seat |
| `min_deposit` | amount | the minimum value of a deposit or fee top-up that the monitor credits |
| `kill_vote_period` | `u64` | the number of subnet blocks for which a kill proposal stays open |
| `kill_delay_period` | `u64` | the number of epochs for which a subnet marked to be killed stays fully operational before its kill checkpoint |

The tag is the ASCII bytes `49 50 43 43 52 54`. Example (stake 1000 sat, `p` = 3, 2 validators, period 10, limit 20, fee 50, two keys, `K` = 144, timeouts 1 and 10, minimum deposit 330 sat, kill proposal period 36, kill delay 5): `49 50 43 43 52 54 e8 07 03 02 0a 14 32 02 ‖ key1 ‖ key2 ‖ 90 01 01 0a ca 02 24 05`, 86 bytes.

[Subnet parameters](../lifecycle.md#subnet-parameters) gives the meaning of every field, [validity of the parameters](../lifecycle.md#validity-of-the-parameters) the conditions the monitor checks, and [choosing the parameter values](../lifecycle.md#choosing-the-parameter-values) the conditions for safety and liveness. The specification gives no values.

## Creating the transaction

The creator chooses parameters that satisfy the [validity conditions](../lifecycle.md#validity-of-the-parameters) and the [conditions for safety and liveness](../lifecycle.md#choosing-the-parameter-values).

The creator's wallet funds and signs the commit transaction, and the creator submits the commit and the reveal together. The provider's `createsubnet` call does this for its wallet and returns the subnet id.

## Verifying the transaction

1. A monitor finds the message in a reveal with exactly one input and one output: the data of the leaf that the input executes starts with `IPCCRT` and decodes.
2. The monitor ignores the message unless the parameters satisfy the [validity conditions](../lifecycle.md#validity-of-the-parameters).
3. The monitor derives the subnet id from the txid of the transaction that carries the message, and ignores the message if a subnet with that id exists. It computes `C_0` and stores the subnet's genesis info ([creating the subnet](../lifecycle.md#creating-the-subnet)).

**Remark.** Two creates get the same subnet id only if the txids of their reveals share the first 20 bytes, which is unlikely. The monitor then keeps the first subnet and ignores the second create.

The message carries no signature, and the whitelist keys need not belong to the creator. If an observer replaces the reveal with its own, the subnet gets another id.
