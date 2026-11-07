# Prefund

With a prefund transaction, a user or a validator deposits BTC before the subnet bootstraps. The monitor credits it to an EVM address as a genesis balance on the subnet.

## Transaction format

One transaction, funded by the depositor's wallet:

| Output | Value | scriptPubKey |
|---|---|---|
| 0 | 0 | OP_RETURN with 46 data bytes |
| 1 | the prefunded amount | `MS(C_wlist)` (**unimplemented**: a P2TR output with a refund path) |
| 2 | wallet change | optional |

OP_RETURN data:

| Offset (bytes) | Size (bytes) | Field |
|---|---|---|
| 0 | 6 | ASCII `IPCPFD` |
| 6 | 20 | `txid20` of the subnet id |
| 26 | 20 | the EVM address to credit |

## Creating the transaction

The depositor builds it while the subnet's status is created, that is before bootstrap ([lifecycle](../lifecycle.md#joins-and-prefunds-before-bootstrap)), with an amount `> 0`. The provider's `prefundsubnet` call does this for its wallet.

## Verifying the transaction

1. A monitor finds the message in output 0: an OP_RETURN with 46 data bytes starting with `IPCPFD`.
2. The monitor ignores the transaction unless the subnet exists and is not bootstrapped, and output 1 pays `MS(C_wlist)` with a value `> 0` (the `MS(C_wlist)` check **unimplemented**).
3. The monitor adds a genesis balance entry (address, amount = output 1's value, txid, height) to the subnet's genesis info.
4. The subnet's genesis balance of each address is the sum of its entries. The subnet starts with these balances, and each address with a balance has an account on the subnet from its first block (genesis balances on the subnet **unimplemented**). The genesis balances add up to the prefunded BTC held by `MS(C_wlist)`. The checkpoints of epoch 0 and the rotation checkpoint of epoch 1 spend that BTC with `C_wlist`'s other funds.

A prefund transaction has no refund path: once a Bitcoin block includes it, only checkpoints spend its BTC (**unimplemented**: the depositor's wallet can also spend output 1 after a relative timelock of 6 Bitcoin blocks).
