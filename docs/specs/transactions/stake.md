# Stake

A validator that has joined a subnet adds a collateral UTXO with a stake transaction (stake transaction **unimplemented**): to raise its power, to stay seated beyond the timeout of its earlier collateral UTXOs, or to return after they matured. The new collateral UTXO is an output under the [collateral script](../encoding.md#collateral-script) with the keys `pk` and `pk_eots` of the validator's join transaction. The message tag is `IPCSUE`.

## Transaction format

A stake transaction is one transaction with an OP_RETURN. A stake transaction that also adds an EOTS nonce commitment, which does not fit in an OP_RETURN, is a commit-reveal pair instead. In both forms, output 1 is the collateral UTXO, under the collateral script of `pk`, `pk_eots`, `T` and the named configuration `C`, which is the current or the previous configuration as for a [join transaction](join.md#transaction-format). Output 2 is optional wallet change, and there are no other outputs. Input 0 may renew a matured collateral UTXO of the validator through its unilateral-exit leaf, with the witness, nLockTime and nSequence of the [exit transaction](exit.md). The other inputs are funds of the validator's wallet.

### Stake without an EOTS nonce commitment

Output 0 is an OP_RETURN with these 62 data bytes:

| Offset (bytes) | Size (bytes) | Field |
|---|---|---|
| 0 | 6 | ASCII `IPCSUE` |
| 6 | 20 | `txid20` of the subnet id |
| 26 | 32 | the validator key `pk`, x-only |
| 58 | 4 | `T`, `u32` little-endian: a Bitcoin block height |

This form carries no signature of the validator: anyone may add a collateral UTXO to a registered validator, and only `pk` can spend it.

### Stake with an EOTS nonce commitment

A [commit-reveal pair](../encoding.md#commit-reveal-pair) as for a [join transaction](join.md#transaction-format): output 0 of the commit transaction is the commitment to `D`. The reveal transaction keeps nLockTime 0 and pays its output to an address of the validator's wallet.

Data `D`:

| Field | Encoding | Meaning |
|---|---|---|
| tag | ASCII `IPCSUE` (6 bytes) | |
| `subnet_id` | subnet id | the subnet |
| `pubkey` | x-only key | the validator key `pk` |
| `timeout` | `u32` | `T`, a Bitcoin block height |
| `nonce_commitment` | `(start: u64, count: u32, root: 32 bytes)` | a new [EOTS nonce commitment](../encoding.md#eots-nonce-commitment) of the `pk_eots` of the join transaction |
| `nonce_commitment_signature` | 64 bytes | BIP340 signature under `pk_eots` on `tagged_sha256("IPCSUE/nonce_commitment", every preceding byte)` |
| `signature` | 64 bytes | BIP340 signature under `pk` on `tagged_sha256("IPCSUE/signature", every preceding byte)` |

The fields from `subnet_id` to `nonce_commitment_signature` are postcard-encoded as one tuple, followed by the raw 64-byte `signature`.

## Creating the transaction

The validator, through its provider:

1. The validator key must have an accepted join transaction in the subnet, and the stake transaction must pass the monitor's rules below.
2. The validator gives the number of epochs for which the collateral UTXO seats it, and the provider sets `T` as for a [join transaction](join.md#creating-the-transaction).
3. To renew a matured collateral UTXO, the provider spends it at input 0 with the validator key. The transaction needs no other funds if the matured collateral UTXO covers the new one and the fee.
4. A validator must add an EOTS nonce commitment, with the second form, before its committed ranges run out. The new range starts after every earlier range of its EOTS key.
5. The provider builds the transaction or the pair with its wallet, checks it against the monitor's rules at the next Bitcoin block height, and submits it.

## Verifying the transaction

1. A monitor finds the message in output 0, an OP_RETURN with 62 data bytes starting with `IPCSUE`, or in a reveal transaction: an input at any index whose executed leaf holds data that starts with `IPCSUE`. For a reveal transaction, the monitor reads the commit transaction spent by that input and requires join rules 2.1 and 2.3, that output 1 is unspent, that the message decodes, that `signature` verifies under `pubkey` and `nonce_commitment_signature` under the `pk_eots` of the validator's join transaction, and `start ≥ 1`, `count ≥ 1` and `start ≥ start' + count'` for every accepted EOTS nonce commitment `(start', count', root')` of that `pk_eots`. Input 0 and output 1 are then those of the commit transaction.
2. The monitor requires, in order:
   1. The subnet exists, and its status is active or to be killed or, for a stake transaction with an EOTS nonce commitment, bootstrapped (epoch 0, with the collateral UTXO naming `C_wlist`). The Bitcoin block is at or above the activation height of the [switch](join.md#activation-of-the-join).
   2. `pk` is the key of an accepted join transaction of the subnet.
   3. Output 1 is locked under the collateral script of `pk`, the `pk_eots` of that join transaction, `T`, and the current or previous configuration.
   4. The value of the collateral UTXO is `≥ min_validator_stake`, `T < 500,000,000`, and `T` satisfies join rule 2.6.
   5. The validator limit and the total power of join rule 3, against the next configuration as filled so far.
3. The monitor rejects a stake transaction that fails any rule: it changes no subnet state. The collateral output stays locked under its script, and its owner spends it through the unilateral-exit leaf after `T`.
4. State changes of an accepted stake transaction: the monitor records the collateral UTXO (outpoint = output 1, value, `T`, key, the backup address of the join transaction, stake txid) and, for the second form, the EOTS nonce commitment. The collateral UTXO seats the validator in the configurations computed at later cutoffs that it qualifies for ([C_e](../lifecycle.md#computing-configuration-c_e-for-e--2)). A collateral UTXO that input 0 spends seats the validator no more.
5. The validators of the named configuration, `C_wlist` in epoch 0, pre-sign the slash transaction of the new collateral UTXO, and a later checkpoint carries the slash pre-signatures ([slashing](../slashing.md#slash-pre-signatures)).
