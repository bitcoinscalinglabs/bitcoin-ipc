# Create subnet

The creator registers a new subnet, its parameters and its configuration `C_wlist`. Anyone can be the creator of a subnet.

## Transaction format

A [commit-reveal pair](../encoding.md#commit-reveal-pair). The commit transaction has no outputs other than the commitment and the funder's change. The reveal transaction has exactly one input, which spends the commitment output, and exactly one output, which pays an address of the creator. A transaction of any other shape is not a create transaction.

Data `D` = `"IPCCRT"` ‖ postcard of these fields, in this order. [Subnet parameters](../lifecycle.md#subnet-parameters) gives their meanings.

| Field | Type |
|---|---|
| `min_validator_stake` | amount |
| `power_unit_exponent` | `u8`, the `p` of `power_unit = 10^p` sat |
| `boot_validators` | `u16` |
| `bottomup_check_period` | `u64` |
| `active_validators_limit` | `u16` |
| `min_cross_msg_fee` | amount |
| `whitelist` | list of x-only keys |
| `K` | `u64` |
| `min_timeout`, `max_timeout` | `u32`, `u32` |
| `min_deposit` | amount |
| `kill_vote_period` | `u64` |
| `kill_delay_period` | `u64` |

The tag is the ASCII bytes `49 50 43 43 52 54`. Example (stake 1000 sat, `p` = 3, 2 validators, period 10, limit 20, fee 50, two keys, `K` = 144, timeouts 1 and 10, minimum deposit 330 sat, kill proposal period 36, kill delay 5): `49 50 43 43 52 54 e8 07 03 02 0a 14 32 02 ‖ key1 ‖ key2 ‖ 90 01 01 0a ca 02 24 05`, 86 bytes.

[Validity of the parameters](../lifecycle.md#validity-of-the-parameters) gives the conditions the monitor checks, and [choosing the parameter values](../lifecycle.md#choosing-the-parameter-values) those for safety and liveness. The specification gives no values.

## Creating the transaction

The creator must choose parameters that satisfy both sets of conditions. Its wallet funds and signs the commit transaction, and it submits the commit transaction and the reveal transaction together. The provider's `createsubnet` call does this and returns the subnet id.

## Verifying the transaction

1. A monitor finds the message in a reveal transaction with exactly one input and one output: the data of the leaf that the input executes starts with `IPCCRT` and decodes.
2. The monitor ignores the message unless the parameters are valid.
3. The monitor derives the [subnet id](../encoding.md#subnet-id) from the txid of the reveal transaction, and ignores the message if a subnet with that id exists. Two create transactions get the same id only if the txids of their reveal transactions share the first 20 bytes, which is unlikely. Otherwise it computes `C_wlist` and stores the subnet's genesis info.

The message carries no signature, and the whitelist keys need not belong to the creator. An observer who replaces the reveal transaction with its own gives the subnet another id.

**Blocking a create.** The reveal transaction must have exactly one input, because two creates in one reveal transaction would get the same id. This lets anyone block a create: an observer rebuilds the reveal transaction with an extra input and gets it mined first. The monitor then ignores the create, the commitment output is spent, and the creator loses the fees and has to create the subnet again.

**Future work.** Derive the subnet id from the txid of the reveal transaction and the index of the input that carries the create. Each create in a reveal transaction then gets its own id, and an extra input no longer blocks a create.
