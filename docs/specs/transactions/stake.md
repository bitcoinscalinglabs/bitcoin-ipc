# Stake

A validator that has joined a subnet adds a collateral with a stake: to raise its power, to stay seated beyond the timeout of its earlier collaterals, or to return after they matured. The new collateral is an output under the [collateral script](../encoding.md#collateral-script), as in a [join](join.md), with the keys of the validator's join. The message tag is `IPCSUE`.

## Transaction format

A stake is one transaction with an OP_RETURN. A stake that also adds an EOTS nonce commitment is a commit-reveal pair instead, because the commitment does not fit in an OP_RETURN.

### Stake without an EOTS nonce commitment

One transaction:

| Input | Spends |
|---|---|
| 0 (optional) | a matured collateral of the validator, through its unilateral-exit leaf, with the witness and nLockTime of the [exit](exit.md) |
| the others | funds of the validator's wallet |

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | 0 | OP_RETURN with 62 data bytes |
| 1 | the collateral | collateral script of `pk`, `pk_eots`, `T` and the named configuration `C` |
| 2 | wallet change | optional |

`pk` and `pk_eots` are the validator key and the EOTS key of the validator's accepted join. The named configuration `C` is the subnet's current or previous configuration: a stake may name the previous configuration for a whole epoch.

OP_RETURN data:

| Offset (bytes) | Size (bytes) | Field |
|---|---|---|
| 0 | 6 | ASCII `IPCSUE` |
| 6 | 20 | `txid20` of the subnet id |
| 26 | 32 | the validator key `pk`, x-only |
| 58 | 4 | `T`, `u32` little-endian: a block height, `T < 500,000,000` |

### Stake with an EOTS nonce commitment

A [commit-reveal pair](../encoding.md#commit-reveal-pair) whose commit transaction carries the collateral, as in a [join](join.md#transaction-format). The commit transaction's input 0 may renew a matured collateral as above. The commit then has the nLockTime and nSequence of the [exit](exit.md), and the reveal keeps nLockTime 0. The commit's outputs are:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | the reveal's output plus its fee | commitment to `D` |
| 1 | the collateral | collateral script of `pk`, `pk_eots`, `T` and the named configuration `C` |
| 2 | wallet change | optional |

The commit transaction has no other outputs. The reveal transaction pays its output to an address of the validator's wallet.

Data `D`:

| Field | Encoding | Meaning |
|---|---|---|
| tag | ASCII `IPCSUE` (6 bytes) | |
| `subnet_id` | subnet id | the subnet |
| `pubkey` | x-only key | the validator key `pk` |
| `timeout` | `u32` | `T`, a block height, `T < 500,000,000` |
| `nonce_commitment` | `(start: u64, count: u32, root: 32 bytes)` | a new [EOTS nonce commitment](../encoding.md#eots-nonce-commitment) of the join's `pk_eots` |
| `nonce_commitment_signature` | 64 bytes | BIP340 signature under `pk_eots` on `tagged_sha256("IPCSUE/nonce_commitment", every preceding byte)` |
| `signature` | 64 bytes | BIP340 signature under `pk` on `tagged_sha256("IPCSUE/signature", every preceding byte)` |

The fields from `subnet_id` to `nonce_commitment_signature` are postcard-encoded as one tuple, and the final 64 bytes are the raw signature.

## Creating the transaction

The validator, through its provider:

1. The subnet's status must be active or to be killed. A stake with an EOTS nonce commitment is also accepted in epoch 0, and its collateral then names `C_0`. The validator key must have an accepted join in the subnet.
2. The collateral must be `≥ min_validator_stake`. The validator gives the number of epochs for which the collateral seats it, and the provider sets `T` from it as for a [join](join.md#creating-the-transaction) (step 5), so that `T` satisfies join rule 2.6.
3. The [switch](join.md#activation-of-the-join) must enable joins at the current tip.
4. To renew a matured collateral, the provider spends it at input 0 with the validator key. The transaction then needs no other funds if the matured collateral covers the new one and the fee.
5. A validator adds an EOTS nonce commitment before its committed ranges run out, with a stake of the second form. The new range starts after every earlier range of its EOTS key.
6. The provider builds the transaction or the pair with its wallet, checks it against the monitor's rules below at the next block height and submits it.

## Verifying the transaction

1. A monitor finds the message in output 0, an OP_RETURN with 62 data bytes starting with `IPCSUE`, or in a reveal: an input at any index whose executed leaf holds data that starts with `IPCSUE`. For a reveal, the monitor reads the commit transaction spent by that input and requires:
   - join rules 2.1 and 2.3, and that output 1 of the commit is unspent,
   - that the message decodes, `signature` verifies under `pubkey` and `nonce_commitment_signature` verifies under the `pk_eots` of the validator's join,
   - `start ≥ 1`, `count ≥ 1`, and `start ≥ start' + count'` for every accepted EOTS nonce commitment `(start', count', root')` of that `pk_eots`.

   In the rules below, input 0 and output 1 are then those of the commit transaction.
2. The monitor requires, in order:
   1. The subnet exists, and its status is active or to be killed, or, for a stake with an EOTS nonce commitment, bootstrapped (epoch 0, with the collateral naming `C_0`). The block is at or above the activation height of the [switch](join.md#activation-of-the-join).
   2. `pk` is the key of an accepted join of the subnet.
   3. Output 1 is locked under the collateral script of `pk`, the `pk_eots` of that join, `T`, and the subnet's current or previous configuration.
   4. The collateral (output 1's value) is `≥ min_validator_stake`, and `T` satisfies join rule 2.6.
   5. The validator limit of join rule 3.
3. The monitor rejects a stake that fails any rule: the stake changes no subnet state. The collateral output stays locked under its script, and its owner spends it through the unilateral-exit leaf after `T`.
4. State changes of an accepted stake: the monitor records the collateral (outpoint = output 1, value, `T`, key, the backup address of the join, stake txid) and, for a stake with an EOTS nonce commitment, the commitment. The collateral seats the validator in the configurations computed at later cutoffs that it qualifies for ([C_e](../lifecycle.md#computing-configuration-c_e-for-e--2)). If input 0 spends an earlier collateral of the validator, that collateral seats the validator no more.
5. The validators of the named configuration pre-sign the slash transaction of the new collateral, and a later checkpoint carries the pre-signatures ([slashing](../slashing.md#after-activation)).

A stake without an EOTS nonce commitment carries no signature of the validator: anyone may add a collateral to a registered validator, and only `pk` can spend it.
