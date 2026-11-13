# Bitcoin-IPC specification

This directory specifies the Bitcoin-IPC protocol, implemented by bitcoin-ipc and ipc: the lifecycle of a subnet and the state every monitor derives from Bitcoin L1, every transaction of the protocol (its format, how its sender builds it and how monitors verify it), and the `ipc-cli` commands with a Bitcoin L1 parent. It says what an implementation must do in every case, so that an engineer can implement the protocol from it and an audit can check the code against it. Each commit holds the specification of that commit, and a tag that of its version.

## Scope

Validators join with their collateral in their own output, exit unilaterally after the collateral UTXO's timeout or earlier with the subnet's consent (cooperative exit), lose their collateral to a slash transaction if they equivocate, and refresh their collateral UTXO once per epoch. The configuration changes only at an epoch cutoff, and the rotation checkpoint of the new epoch moves the subnet's funds to it. A checkpoint spends only outputs of final transactions, except the change output of the previous checkpoint.

## Conventions

- "Bitcoin L1" is the Bitcoin network a deployment runs on. Network-specific values are [parameters](lifecycle.md#parameters).
- A rule written with "must" is a requirement on every implementation. The monitor ignores a message that breaks a rule, and that message changes no state. The monitor still records every spend of an output it holds.
- Names in code font are the code's names where they exist.
- A note "(… **unimplemented**)" marks a part of this specification that the code does not implement yet, once per item. Every rule that depends on an unimplemented item is unimplemented too. A search for `**unimplemented**` lists them all.

## Documents

- [lifecycle.md](lifecycle.md): actors, parameters, the lifecycle of a subnet with its configurations (`C_wlist`, `C_gen`, `C_1`, `C_e`), subnet status, the monitor's state and processing, and the conditions on the parameter values.
- [modules.md](modules.md): the programs and the contract that implement the protocol, the IPC-aware node, what each module holds and trusts.
- [slashing.md](slashing.md): EOTS keys, EOTS nonce commitments and EOTS votes, final subnet blocks and the subnet final height, slash pre-signatures and the checkpoints that carry them, and the slashing of a validator that equivocated.
- [encoding.md](encoding.md): fee rate and dust, subnet ids, postcard encoding, OP_RETURN outputs, commit-reveal pairs, configuration multisigs, the collateral and unbonding scripts, EOTS nonce commitments and EOTS votes.
- Transactions, in lifecycle order:
  - [create-subnet.md](transactions/create-subnet.md): create a subnet (`IPCCRT`).
  - [prefund.md](transactions/prefund.md): deposit before bootstrap into the genesis balance (`IPCPFD`).
  - [join.md](transactions/join.md): join with the collateral in the validator's own output (`IPCJUE`).
  - [stake.md](transactions/stake.md): add or renew a collateral UTXO of a validator that has joined (`IPCSUE`).
  - [deposit.md](transactions/deposit.md): deposit and fee top-up (`IPCFND`).
  - [checkpoint.md](transactions/checkpoint.md): the checkpoint, including the checkpoints of epoch 0 and the kill checkpoint (`IPCCPT`).
  - [batch-transfer.md](transactions/batch-transfer.md): the checkpoint's reveal transaction with transfers, ERC20 records and slash pre-signatures (`IPCTFR`).
  - [exit.md](transactions/exit.md): spend matured collateral UTXOs and unbonding outputs (no tag).
  - [slash.md](transactions/slash.md): burn the collateral UTXO of an equivocating validator (no tag).
- [erc20.md](erc20.md): registration of an ERC20 token, how it reaches other subnets, transfers and supply accounting.
- [ipc-cli.md](ipc-cli.md): `ipc-cli` commands with a Bitcoin L1 parent and the provider methods they call.
- [code-map.md](code-map.md): where the code lives and the interfaces between its parts.
