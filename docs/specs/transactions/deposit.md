# Deposit and fee top-up

With a deposit transaction, a user sends BTC into a subnet, which credits it to an EVM address (a top-down fund message). With a fee top-up transaction, a validator adds BTC to the subnet's fee reserve and credits nobody (fee top-up **unimplemented**). The code calls the deposit "fund" (tag `IPCFND`).

## Transaction format

One transaction, funded by the depositor's wallet:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | 0 | OP_RETURN with 46 data bytes (deposit) or 26 data bytes (fee top-up) |
| 1 | the deposited amount | `MS(C)` of the subnet's current configuration `C` |
| 2 | wallet change | optional |

OP_RETURN data:

| Offset (bytes) | Size (bytes) | Field |
|---|---|---|
| 0 | 6 | ASCII `IPCFND` |
| 6 | 20 | `txid20` of the subnet id |
| 26 | 20 | the EVM address to credit (deposit only) |

## Creating the transaction

The provider's `fundsubnet` call builds it with its wallet, paying the multisig of the current configuration (`C_wlist` in epoch 0), for a subnet in a status that rule 2 allows.

## Verifying the transaction

1. A monitor finds the message in output 0: an OP_RETURN with 46 or 26 data bytes starting with `IPCFND`.
2. The subnet must be in epoch 0 or active for a deposit transaction, and in epoch 0, active or to be killed for a fee top-up transaction.
3. Output 1 must have a value `≥ min_deposit` and pay `MS(C)` (the `MS(C)` check **unimplemented**) of the current configuration (`C_wlist` in epoch 0), or of an earlier one within its [delayed-arrival window](checkpoint.md#fund-utxos). The monitor ignores any other deposit or fee top-up transaction.
4. The monitor adds output 1 to the fund UTXOs of that configuration, which checkpoints spend. For a deposit transaction, it records a fund rootnet message for the subnet: the address, the amount (output 1's value), the next message nonce of the subnet and the Bitcoin block that includes the transaction. The subnet credits the amount once its parent finality reaches that Bitcoin block, in epoch 0 as after activation. Deposits back the BTC that the subnet mints. A fee top-up transaction records no message and adds its value to the fee reserve, which alone pays for checkpoints.

Before bootstrap, BTC enters the subnet only through a [prefund transaction](prefund.md). The deposit and fee top-up transactions of epoch 0 are fund UTXOs of `C_wlist`, which the checkpoints of epoch 0 and the rotation checkpoint of epoch 1 spend.
