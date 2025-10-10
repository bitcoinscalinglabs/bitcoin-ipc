## Attaching arbitrary data to bitcoin transactions

At some points in the protocol we need to attach arbitrary data to a bitcoin transaction, for example to describe the destination subnet and address of a transfer.
This is done using the script-spend path of a taproot transaction. The technique, also known as the "commit-reveal method" requires submitting two transactions to bitcoin.

We model this as a functionality **writeArbitraryData(in, out, data)**, where *in* and *out* are the input and output UTXOS, respectively, and *data* some arbitrary data. The functionality is implemented by **two** bitcoin transactions.
Specifically, the following steps are taken:
- Create a script containing *data*.
- Create **checkpointTx**, the first Bitcoin transaction, that spends the UTXO(s) *in* and creates the output UTXOs *out* and an UTXO *temp*, which contains the hash of the script.
- Create **batchTransferTx**, the second bitcoin transaction, that spends *temp* by revealing the content of the script as the *witness*.
- Observe that *data* is submitted with a lower transaction fee as a SegWit witness.
- These two bitcoin transactions are submitted to Bitcoin by the same entity (e.g., a subnet validator or a relayer).

The implementation uses the **writeArbitraryData()** functionality to implement multiple IPC commands, such as subnet creation, validators joining a subnet, checkpointing, and cross-subnet transfer. For example, this technique allows us to batch thousands of transfers within two bitcoin transactions: the **checkpointTx** contains an input UTXO, taking the funds from the source subnet, and one output UTXO for each target subnet, while the **batchTransferTx** reveals the actual recipient address in each subnet.

## Subnet ID

We follow the [IPC Specs](https://github.com/consensus-shipyard/ipc/blob/main/specs/addressing.md) and uniquely identify an IPC subnet by a `subnetId`, which is a list consisting of the `SubnetAddress` of each subnet in the hierarchy from the root subnet to the subnet of interest.
The string representation of the subnet ID uses `/` as a divider.

> **Example:**
>
> An L2 subnetId over Bitcoin Mainnet can be:
>
> `/b1/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei`.

The root subnet is identified by one of the following strings: `b1` (mainnet), `b2` (testnet), `b22` (testnet4), `b3` (signet), `b4` (regtest). This corresponds to the Bitcoin network serving as the root of the subnets.

The `t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei` part is a value in the format of an [FVM delegated address](https://docs.filecoin.io/smart-contracts/filecoin-evm-runtime/address-types#delegated-addresses), which is the variant of a Filecoin address used to represent foreign addressing systems. It is using the namespace value of `10` to match IPC subnet smart contracts.

It encodes the first 20 bytes of the transaction hash (id) of the Bitcoin transaction that created the subnet, thus making the subnetId unique. The FVM address can be decoded back to the original 20 bytes of the transaction hash.

For L3+ subnets we keep the addressing mechanism used in Fendermint. The subnet address of an L3, for example,
will be assigned by its parent L2 subnet, hence we let the existing Fendermint L2 implementation handle it.
