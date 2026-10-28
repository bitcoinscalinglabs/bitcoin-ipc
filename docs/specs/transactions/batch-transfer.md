# Batch transfer

The reveal of a checkpoint's commitment output. It carries the data that does not fit in the checkpoint's OP_RETURN: the recipients of the outgoing BTC transfers, the ERC20 records (token registrations, supply adjustments and transfers) and the slash pre-signatures. A relayer submits it together with its checkpoint. The code calls it the batch transfer (tag `IPCTFR`).

## Transaction format

| Field | Value |
|---|---|
| version | 2 |
| nLockTime | 0 |
| input 0 | the checkpoint's commitment output (index `p + W + 1`, with `p` and `W` as in the [checkpoint format](checkpoint.md#transaction-format)). Witness `[S, cb]` with `S` the data leaf script of `D` ([commit-reveal pair](../encoding.md#commit-reveal-pair)). nSequence `0xffffffff` |
| output 0 | `max(110·r, 330)` sat to `MS` of the checkpoint's configuration |

Data `D` = `"IPCTFR"` ‖ postcard of:

| Field | Encoding |
|---|---|
| `transfers` | the transfer batches: a map from the destination subnet's `txid20` to the list of its native transfers, each an EVM recipient and an amount |
| `token_registrations` | list of `(home_token_address: EVM address, name: string, symbol: string, decimals: u8, initial_supply: compact U256)` |
| `token_supply_adjustments` | list of `(home_token_address: EVM address, delta: compact I256)`, `> 0` for a mint, `< 0` for a burn |
| `token_transfers` | map from the destination subnet's `txid20` to the list of its ERC20 transfers, each `(home_subnet_id: subnet id, home_token_address: EVM address, amount: compact U256, recipient: EVM address)` |
| `slash_signatures` | the [slash](slash.md) pre-signatures: a list of entries `(txid: 32 bytes, vout: u32, signatures: list of (index: u16, signature: 64 bytes))`, one per collateral or unbonding output, in ascending order of `(txid, vout)`. `(txid, vout)` is the output's outpoint, with the txid in internal byte order (the reverse of its hex form). Each signature is the BIP340 signature `s_i` of the slash transaction's witness, and `index` is `i − 1`, the position of the signer's key in the ascending order of the keys of the configuration that the output's slash leaf names. The signatures are in ascending order of `index`, at most one per index |
| `h_0` | option of `u64`: `H_0` in the first checkpoint's data, absent in every other checkpoint |

Map entries are in ascending order of `txid20`. All fields are present, empty or not. The home subnet of the registrations and supply adjustments is the checkpoint's subnet.

## Creating the transaction

Every provider builds it together with its checkpoint, from the same data, after checking the ERC20 records against the [ERC20 rules](#verifying-the-transaction) in its monitor's state. It needs no signature. The relayer submits it with the checkpoint once the checkpoint carries the signatures of the configurations that lock its inputs.

## Verifying the transaction

1. A monitor finds the message in a reveal that spends the commitment output of a checkpoint it has processed, in this block or an earlier one: the data of the leaf executed by that input starts with `IPCTFR` and decodes.
2. Every native transfer has an amount `> 0` and a non-zero recipient. Every registration has a non-zero token address and a non-empty name and symbol. Every ERC20 transfer has a non-zero token address, a non-zero recipient and an amount `> 0`. Every entry of `slash_signatures` satisfies rule 1 of [slashable outputs](../slashing.md#slashable-outputs). At least one of the lists and maps is not empty, or `h_0` is present.
3. Native transfers. The number of destination subnets in `transfers` must equal the checkpoint's `B` ([OP_RETURN data](checkpoint.md#op_return-data)). For every destination, the checkpoint must have an output of at least the dust limit, with the sum of its transfers, that pays the multisig of the destination's current configuration. The output may instead pay the multisig of an earlier configuration `C_e` (`e ≥ 0`) of the destination while the height of the block that includes the checkpoint is at most `h_e + K/2`, with `h_0 = H_0` for `C_0` ([delayed arrivals](checkpoint.md#fund-utxos)). The monitor credits the transfers only to a destination whose status is active or to be killed.
4. ERC20 rules, applied in this order with running balances (the balance of a token held by a subnet):
   1. Registrations: the batch registers each token at most once, and the token has no balance on its home subnet yet. Its home subnet's balance becomes `initial_supply`.
   2. Supply adjustments: the monitor adds the mints to the home subnet's balance. The burns must then be `≤` that balance.
   3. ERC20 transfers: the amount must be `≤` the source subnet's balance of that token. The monitor applies an ERC20 transfer only to a destination whose status is active or to be killed. For any other destination it applies nothing, and the amount stays with the source.

   On a violation, the monitor rejects the whole batch transfer.
5. The monitor then applies these state changes together. It numbers the rootnet messages in this order: the native transfers (destinations in ascending order of `txid20`, each list in its order), the registrations, then the ERC20 transfers (in the order of the native transfers).
   - It adds the batch transfer's txid and height to the checkpoint record.
   - It records each native transfer as a fund rootnet message for its destination subnet.
   - It records each registration as an ERC20 registration rootnet message for every other subnet.
   - It changes the balances as in step 4, moving the amount of each ERC20 transfer from the source subnet's balance to the destination's, and records each ERC20 transfer as an ERC20 transfer rootnet message for the destination.
   - It records the slash pre-signatures ([slashable outputs](../slashing.md#slashable-outputs)).
   - If output 0 of the batch transfer pays `MS` of the checkpoint's configuration, it adds that output to the fund UTXOs of that configuration and its value to the fee reserve.
   - For the first checkpoint, it records `H_0` and computes `C_1`, and the subnet activates ([activation](../lifecycle.md#activation)). The first checkpoint's data must carry `h_0`, and no other checkpoint's data may.

A batch transfer that fails a rule changes nothing: the monitor applies none of its data, neither the transfers of any destination nor the ERC20 records nor the slash pre-signatures. Its checkpoint keeps its effect, except the first checkpoint, which then has no effect beyond the spends that the monitor records.
