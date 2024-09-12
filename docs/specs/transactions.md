## Preliminaries
- We identify L2 subnets using their `subnetAddress`, see `addressing.md`.

## Attach arbitrary data to bitcoin transactions
This will be done using the script-path of a taproot transaction.

We model this as a functionality **writeArbitraryData(in, out, data**), where *in* and *out* are the input and output UTXOS, respectively, and *data* some arbitrary data. The functionality is implemented by **two** bitcoin transactions. Specifically:
- Create a script containing *data*.
- Create **commitTx**, the first Bitcoin transaction, that spends the UTXO(s) *in* and creates an output UTXO *temp* that contains a hash of the script.
- Create **revealTx**, the second bitcoin transaction, that spends *temp* by revealing the content of the script as the *witness,* and outputs *out* as the output UTXO.
    - Observe that *data* is submitted with a lower transaction fee as witness.
- These two bitcoin transactions are submitted to Bitcoin by the same entity.

We will be using the **writeArbitraryData()** functionality to implement IPC commands such as *createChild* and *joinChild*.


## Create child subnet
This command allows IPC-aware nodes (see `architecture.md`) to become validators in an IPC L2 subnet.

We model this as a functionality *createChild(subnetData)*:

- *subnetData* contains the following data:
    - A known tag that this transaction is about creating a new IPC Subnet.
    - The designated number of subnet validators and the required collateral for each.
    - Possibly more arbitrary data.
- The functionality will be implemented using the *writeArbitraryData(in, out, data)* functionality, where:
    - *in*: UTXO(s) spendable by the wallet that submits *createChild()*
    - *out*: a UTXO with some amount locked by *subnetPK*, which will be used to pay the transactions fees in later stages.
    - *data*: *subnetData*

Here is the flow of a subnet creation.
![Create Subnet](../diagrams/create-subnet.png)

Observe in the diagram that *subnetPK* is computed locally at the machine of the process that submits the *createChild()* command. In later stages this will be replaced by an interactive protocol. See `subnet-pk.md` for more explanation.

## Join subnet
We model this as a functionality *joinChild(subnetAddress, validatorData)*:
- *validatorData* contains validator’s info, such as their IP, to allow discovery from other validators of the subnet
- It is implemented using the *writeArbitraryData(in, out, data)* functionality, where:
    - *in*: UTXO(s) spendable by the node that wants to become a validator
    - *out*: a UTXO with collateral BTC locked by *subnetPK*
    - *data*: *validatorData*


## Deposit
This command allows subnet users to deposit funds from their bitcoin wallet to their subnet address (denoted *userAddress*). Specifically, they can "lock" an amount of BTC on L1 and "mine" an equal amount of *wrapped BTC* on the L2 subnet.

We model this as a functionality *deposit(subnetAddress, amount, userAddress)*.
It is implemented as a single bitcoin transaction with the following inputs and outputs:
- inputs: One or more UTXOs, spendable by the user's wallet, with total value the desired *amount* plus the miner's fee.
- outputs: (1) A UTXO of value *V*, locked with the *subnetPK* that corresponds to *subnetAddress*. (2) A UTXO with 0 value, containing the OP_RETURN opcode and *userAddress*.
The function is signed and submitted by the user. 

Prior to creating this transaction, the user locally creates a secret key and obtains a *userAddress* for the subnet.

