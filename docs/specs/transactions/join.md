# Join

A validator joins a subnet with a collateral in its own output under the [collateral script](../encoding.md#collateral-script). Only the validator can spend it, through the unilateral-exit leaf after its timeout `T`, and the [slash transaction](slash.md) burns it if the validator equivocates. The code calls this transaction join-ue (message tag `IPCJUE`).

## Transaction format

A [commit-reveal pair](../encoding.md#commit-reveal-pair) whose commit transaction carries the collateral.

Commit transaction outputs:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | the reveal transaction's output plus its fee | commitment to `D` |
| 1 | the collateral | collateral script of `pk`, `pk_eots`, `T` and the named configuration `C` |
| 2 | wallet change | optional |

The commit transaction has no other outputs. The named configuration `C` is `C_wlist` before activation, and after activation the subnet's current or previous configuration: a join transaction may name the previous configuration for a whole epoch.

The reveal transaction pays its output to an address of the validator's wallet.

Data `D`:

| Field | Encoding | Meaning |
|---|---|---|
| tag | ASCII `IPCJUE` (6 bytes) | |
| `subnet_id` | subnet id | the subnet to join |
| `ip` | socket address | the validator's network address |
| `backup_address` | Bitcoin address | where the exit transaction pays the collateral |
| `pubkey` | x-only key | the validator key `pk` (consensus key and Bitcoin key) |
| `timeout` | `u32` | `T`, a block height, `T < 500,000,000` |
| `pk_eots` | x-only key | the [EOTS](../encoding.md#notation) key used for slashing |
| `nonce_commitment` | `(start: u64, count: u32, root: 32 bytes)` | the [EOTS nonce commitment](../encoding.md#eots-nonce-commitment) of `pk_eots` for subnet heights `start ≤ h < start + count` |
| `nonce_commitment_signature` | 64 bytes | BIP340 signature under `pk_eots` on `tagged_sha256("IPCJUE/nonce_commitment", every preceding byte)` |
| `signature` | 64 bytes | BIP340 signature under `pk` on `tagged_sha256("IPCJUE/signature", every preceding byte)` |

[Encoding](../encoding.md#notation) defines `tagged_sha256`. The fields from `subnet_id` to `nonce_commitment_signature` are postcard-encoded as one tuple, and the final 64 bytes are the raw signature.

## Creating the transaction

The validator, through its provider's `joinsubnet` call with `epochs`:

1. The subnet must exist, and the validator submits the join transaction only before bootstrap or after activation. The key must not have joined the subnet before. Before bootstrap it must be in the whitelist. After activation the subnet's status must be active or to be killed.
2. The collateral must be `≥ min_validator_stake`.
3. The switch of the last section must enable join transactions at the current tip.
4. The key `pk` must be the provider's own validator key, because the message carries its signature.
5. The validator gives `epochs`, the number `n` of epochs for which the collateral seats it, with `min_timeout ≤ n ≤ max_timeout`. The provider sets `T = H_0 + (e + n + 3) · K`, the last block of epoch `e + n + 3`, where `e` is the subnet's current epoch. Before activation, `e = 0` and the latest final Bitcoin L1 height takes the place of `H_0`. This `T` satisfies rule 2.6, and the collateral seats the validator in at most `n` configurations, `C_{e+1}` to `C_{e+n}`. `T` must be `< 500,000,000`.
6. The validator generates its EOTS key and nonce seed locally ([command](../ipc-cli.md#generate-the-eots-key)), and its provider holds both. The provider sets `pk_eots` and the EOTS nonce commitment of the message from them.
7. The provider signs the message, builds the pair with its wallet, checks it against the monitor's rules below at the next block height and submits both transactions together.

The validator's subnet address needs an account on the subnet before the validator can broadcast its checkpoint signatures and slash pre-signatures. It gets one as a member of `C_wlist` (a genesis account), from a [prefund transaction](prefund.md) to that address (a genesis balance), or from a [deposit transaction](deposit.md) to it, submitted in epoch 0 or later. The subnet credits a deposit transaction once its parent finality reaches the Bitcoin block that includes it. Validators submit their signatures on the subnet without fees.

## Verifying the transaction

1. A monitor finds the message in a reveal transaction: an input whose executed leaf holds data that starts with `IPCJUE`. The input may be at any index. The monitor processes every such input of a transaction, in input order, as a separate join transaction. It ignores every join transaction in a block below the activation height of the switch.
2. The monitor reads the commit transaction spent by that input (in the same block or an earlier one) and requires, in order:
   1. The input spends output 0 of the commit transaction, a P2TR output, through a single leaf that is exactly the data leaf script of the message.
   2. The message decodes, `signature` verifies under `pubkey`, `nonce_commitment_signature` verifies under `pk_eots`, `pk_eots ≠ pubkey`, and the EOTS nonce commitment has `start ≥ 1` and `count ≥ 1`.
   3. The commit transaction has 2 or 3 outputs.
   4. Output 1 is locked under the collateral script of the message's `pubkey`, `pk_eots` and `T`, naming the configuration required above.
   5. Output 1 is unspent at the reveal transaction: neither another input of the reveal transaction nor a transaction before the reveal transaction spends it, where the transactions before it are those before it in its Bitcoin block and those in the Bitcoin blocks from the commit transaction's on. Spends after the reveal transaction do not count. The collateral (output 1's value) is `> 0`, the backup address is valid for the network, and `T < 500,000,000`.
   6. `block height + 1 < T`, and the epoch `E_T` that contains `T` satisfies `e + min_timeout + 3 ≤ E_T ≤ e + max_timeout + 3`, where `e` is the subnet's current epoch and `E_T = ⌈(T − H_0) / K⌉` ([epochs](../lifecycle.md#epochs)). Before activation, `e = 0` and the height of the block that includes the join transaction takes the place of `H_0`.
   7. No earlier accepted join transaction carries `pk_eots`, and no earlier accepted join transaction of the subnet carries `pubkey`: a validator key joins a subnet at most once. Afterwards the validator adds collateral with a [stake transaction](stake.md).
3. Then the admission rules:
   - The subnet exists.
   - Before bootstrap: the collateral is `≥ min_validator_stake` and the key is in the whitelist.
   - After activation: the subnet's status is active or to be killed, and the collateral is `≥ min_validator_stake`.
   - The validator limit (**unimplemented**): if `pk` holds no collateral that counts, fewer than `active_validators_limit` validators hold one. A collateral counts if it is unspent, its timeout is `> H_{e+3}`, and it is locked under the configuration of the current epoch `e` or of one of the two epochs before: `C_e`, `C_{e−1}` or `C_{e−2}`, with `C_wlist` for epoch 0. Before activation, the collateral must be locked under `C_wlist` instead, and `H_{e+3}` is computed from the height of the block that includes the join transaction in place of `H_0`.
   - The total power: with the new collateral, the validators that hold a collateral that counts have a total power `≤ MAX_TOTAL_POWER` ([power and thresholds](../lifecycle.md#power-and-thresholds)).
   - Between bootstrap and activation (epoch 0), the monitor accepts no join transaction.
4. The monitor rejects a join transaction that fails any rule: it changes no subnet state, and the monitor stores nothing for it. The collateral output stays locked under its script, and its owner spends it through the unilateral-exit leaf after `T`, with a transaction it builds by hand.
5. State changes of an accepted join transaction:
   - The monitor records the collateral (outpoint = commit output 1, value, `T`, key, backup address, join txid = reveal txid).
   - Before bootstrap, the monitor adds the join transaction to the accepted join transactions of the genesis info. The `boot_validators`-th accepted join transaction bootstraps the subnet ([C_gen](../lifecycle.md#computing-configuration-c_gen)).
   - After activation, the collateral seats the validator in the configurations computed at later cutoffs that it qualifies for.
6. The validators of the named configuration pre-sign the slash transaction of the new collateral, and a later checkpoint carries the slash pre-signatures ([slashing](../slashing.md#after-activation)).

The message does not bind the transaction that funds the collateral: anyone can copy a join transaction's data into another commit-reveal pair whose output 1 has the same collateral script. The monitor accepts whichever it processes first and rejects the other by rule 2.7. Only `pk` can spend the accepted collateral through the unilateral-exit leaf, so the copier gains nothing.

The collateral stops seating the validator before it matures. From block `T + 1` on, the validator spends it with the [exit transaction](exit.md).

## Activation of the join

A deployment enables join transactions from the height `UE_ACTIVATION_HEIGHT`. When the value is unset or empty, the deployment accepts no join transaction. Monitors ignore join messages in blocks below that height, and providers refuse join and exit transactions while the tip is below it. All monitors of a deployment must use the same value. A monitor records the value it runs with. At start it accepts a value different from the recorded one only if both values are above the last processed height, and then records the new value. An unset value counts as above every height. A monitor with no processed block accepts any value.
