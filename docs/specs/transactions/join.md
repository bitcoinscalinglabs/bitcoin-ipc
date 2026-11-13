# Join

A validator joins a subnet with a collateral in its own output under the [collateral script](../encoding.md#collateral-script). Only the validator can spend it, through the unilateral-exit leaf after its timeout `T`, and the [slash transaction](slash.md) burns it if the validator equivocates. The code calls this transaction join-ue (message tag `IPCJUE`).

## Transaction format

A [commit-reveal pair](../encoding.md#commit-reveal-pair) whose commit transaction carries the collateral UTXO and has only these outputs:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | the reveal transaction's output plus its fee | commitment to `D` |
| 1 | the collateral UTXO | collateral script of `pk`, `pk_eots`, `T` and the named configuration `C` |
| 2 | wallet change | optional |

The named configuration `C` is `C_wlist` before activation. After activation it is the current or the previous configuration when the validator creates the join transaction: a join transaction may name the previous configuration for a whole epoch. The reveal transaction pays its output to an address of the validator's wallet.

Data `D`:

| Field | Encoding | Meaning |
|---|---|---|
| tag | ASCII `IPCJUE` (6 bytes) | |
| `subnet_id` | subnet id | the subnet to join |
| `ip` | socket address | the validator's network address |
| `backup_address` | Bitcoin address | where the exit transaction pays the collateral |
| `pubkey` | x-only key | the validator key `pk` (consensus key and Bitcoin key) |
| `timeout` | `u32` | `T`, a Bitcoin block height |
| `pk_eots` | x-only key | the [EOTS](../encoding.md#notation) key used for slashing |
| `nonce_commitment` | `(start: u64, count: u32, root: 32 bytes)` | the [EOTS nonce commitment](../encoding.md#eots-nonce-commitment) of `pk_eots` for subnet heights `start ≤ h < start + count` |
| `nonce_commitment_signature` | 64 bytes | BIP340 signature under `pk_eots` on `tagged_sha256("IPCJUE/nonce_commitment", every preceding byte)` |
| `signature` | 64 bytes | BIP340 signature under `pk` on `tagged_sha256("IPCJUE/signature", every preceding byte)` |

The fields from `subnet_id` to `nonce_commitment_signature` are postcard-encoded as one tuple, followed by the raw 64-byte `signature`. [Encoding](../encoding.md#notation) defines `tagged_sha256`.

## Creating the transaction

The validator calls its provider's `joinsubnet` with `epochs`, the number `n` of epochs for which the collateral UTXO seats it, with `min_timeout ≤ n ≤ max_timeout`. The provider runs one join at a time, and keeps answering its other methods meanwhile. It refuses the join, before any transaction, while its monitor has not processed the Bitcoin chain up to the node's tip, read once the join is the one running and again before its first transaction is sent (its accepted joins and EOTS keys are incomplete until then), when an accepted join transaction of the subnet carries its validator key, or when the join transaction it last sent to the subnet is in the mempool or in a Bitcoin block the monitor has not processed. A join transaction that was dropped or that the monitor rejected lets the next join through, with the same EOTS key. A reveal transaction that did not reach the node while its commit transaction did is sent again. If another transaction spent its input instead, the join is refused until the monitor has processed that transaction, and then a new pair is sent. Otherwise the provider:

1. Sets `T = H_0 + (e + n + 3) · K`, the last Bitcoin block of epoch `e + n + 3`, where `e` is the current epoch. Before activation, `e = 0` and the latest final Bitcoin L1 height takes the place of `H_0`. This `T` satisfies rule 2.6, and the collateral UTXO seats the validator in at most `n` configurations, `C_{e+1}` to `C_{e+n}`.
2. Uses its own validator key as `pk`, because the message carries its signature, and sets `pk_eots` and the EOTS nonce commitment from the subnet's EOTS key and nonce seed, which it generates at the first join to the subnet, the nonce seed before the key ([EOTS key](../slashing.md#eots-key-and-eots-nonce-commitments)). A key configured with `EOTS_SK_PATH` is bound to the first subnet it is chosen for, before any transaction, and stays that subnet's key across failures and restarts.
3. Builds the prefund transaction of `--initial-balance`, if any, records it for the subnet and submits it. The record keeps it until a join to the subnet is accepted: a later join never sends a second prefund transaction, whatever happened to the recorded one, and names it. A join that fails after recording the prefund transaction reports its txid and that the join is incomplete; a prefund transaction that never reached Bitcoin is sent with `pre-fund` ([ipc-cli](../ipc-cli.md#prefund-the-genesis-balance)). Then builds the pair with its wallet, checks it against the monitor's rules below at the next Bitcoin block height, records it for the subnet before sending, and submits both transactions together.

The validator's subnet address needs an account on the subnet to broadcast its checkpoint signatures, Bitcoin signatures and slash pre-signatures, which cost no fee. It has one as a member of `C_wlist`, or gets one from a [prefund transaction](prefund.md) or a [deposit transaction](deposit.md) to that address. The subnet credits a deposit transaction once its parent finality reaches the Bitcoin block that includes it.

## Verifying the transaction

1. A monitor finds the message in a reveal transaction: an input, at any index, whose executed leaf holds data that starts with `IPCJUE`. It processes every such input of a transaction, in input order, as a separate join transaction. It ignores every join transaction in a Bitcoin block below the activation height of the [switch](#activation-of-the-join).
2. The monitor reads the commit transaction spent by that input, in the same Bitcoin block or an earlier one, and requires, in order:
   1. The input spends output 0 of the commit transaction, a P2TR output, through a single leaf that is exactly the data leaf script of the message.
   2. The message decodes, `signature` verifies under `pubkey`, `nonce_commitment_signature` verifies under `pk_eots`, `pk_eots ≠ pubkey`, and the EOTS nonce commitment has `start ≥ 1` and `count ≥ 1`.
   3. The commit transaction has 2 or 3 outputs.
   4. Output 1 is locked under the collateral script of the message's `pubkey`, `pk_eots` and `T`, naming the configuration required above.
   5. Output 1 is unspent at the reveal transaction: neither another input of the reveal transaction nor a transaction before it spends it. The transactions before it are those before it in its Bitcoin block and those in the Bitcoin blocks from the commit transaction's on. The value of the collateral UTXO is `> 0`, the backup address is valid for the network, and `T < 500,000,000`.
   6. `h + 1 < T`, with `h` the height of the Bitcoin block that includes the join transaction, and the epoch `E_T = ⌈(T − H_0) / K⌉` that contains `T` satisfies `e + min_timeout + 3 ≤ E_T ≤ e + max_timeout + 3`, where `e` is the current epoch ([epochs](../lifecycle.md#epochs)). Before activation, `e = 0` and `h` takes the place of `H_0`.
   7. No earlier accepted join transaction carries `pk_eots`, and no earlier accepted join transaction of the subnet carries `pubkey`: a validator key joins a subnet at most once, and adds collateral with a [stake transaction](stake.md).
3. Then:
   - The subnet exists. Before bootstrap, the key is in the whitelist. After activation, the subnet's status is active or to be killed. Between bootstrap and activation (epoch 0), the monitor accepts no join transaction.
   - The value of the collateral UTXO is `≥ min_validator_stake`.
   - The monitor checks the join transaction against the [next configuration as filled so far](../lifecycle.md#epochs): the validators that hold a *collateral UTXO that counts*, one that is unspent, has a timeout `> H_{e+3}` and is locked under `C_e`, `C_{e−1}` or `C_{e−2}` (`C_wlist` for epoch 0). Before activation, the collateral UTXO must be locked under `C_wlist`, and `H_{e+3}` is computed with `h` in place of `H_0`.
     - The validator limit (**unimplemented**): if `pk` holds no collateral UTXO that counts, fewer than `active_validators_limit` validators hold one.
     - The total power: with the new collateral UTXO, the validators that hold a collateral UTXO that counts have a total power `≤ MAX_TOTAL_POWER` ([power and thresholds](../lifecycle.md#power-and-thresholds)).
4. The monitor rejects a join transaction that fails any rule: it changes no subnet state and stores nothing for it. The collateral output stays locked under its script, and its owner spends it through the unilateral-exit leaf after `T`, with a transaction it builds by hand.
5. State changes of an accepted join transaction:
   - The monitor records the collateral UTXO (outpoint = output 1 of the commit transaction, value, `T`, key, backup address, join txid = txid of the reveal transaction).
   - Before bootstrap, it adds the join transaction to the accepted join transactions of the genesis info. The `boot_validators`-th one bootstraps the subnet ([C_gen](../lifecycle.md#computing-configuration-c_gen)).
   - After activation, the collateral UTXO seats the validator in the configurations computed at later cutoffs that it qualifies for.
6. The validators of the named configuration pre-sign the slash transaction of the new collateral UTXO, and a later checkpoint carries the slash pre-signatures ([slashing](../slashing.md#after-activation)).

The message does not bind the transaction that funds the collateral UTXO: anyone can copy a join transaction's data into another commit-reveal pair whose output 1 has the same collateral script. The monitor accepts whichever it processes first and rejects the other by rule 2.7. Only `pk` can spend the accepted collateral UTXO, so the copier gains nothing.

The collateral UTXO stops seating the validator before it matures. From Bitcoin block `T + 1` on, the validator spends it with the [exit transaction](exit.md).

## Activation of the join

A deployment enables join transactions from the height `UE_ACTIVATION_HEIGHT`. With the value unset or empty, it accepts none. Monitors ignore join messages in Bitcoin blocks below that height, and providers refuse join and exit transactions while the tip is below it. All monitors of a deployment must use the same value. A monitor records the value it runs with. At start, it accepts a value different from the recorded one only if both are above the last processed height, and then records the new value. An unset value counts as above every height. A monitor with no processed Bitcoin block accepts any value.
