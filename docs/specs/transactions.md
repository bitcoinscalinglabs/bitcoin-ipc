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

Specifically, the user that wants to create an IPC subnet does the following:
- create a bitcoin key pair, let `subnetPK` and `subnetSK` be the public and secret key, respectively
- store locally the  `subnetSK`
- use `subnetPK` when calling `createChild()`

The btc_monitor is responsible for detecting a createChild transaction. The btc monitor pools the chain 
for newly produced blocks and checks if a transaction contains an IPC create command in the witness.
If the IPC create command keyword is detected in the witness, the btc_monitor extracts the other parameters
encoded in the commit-reveal transaction and creates a new subnet entity.

Observe in the diagram that *subnetPK* is computed locally at the machine of the process that submits the *createChild()* command. The *subnetSK* will be used when a signature from the subnet is required.
In Stage 3 this will be replaced by an interactive protocol. See `subnet-pk.md` for more explanation.

## Join subnet
We model this as a functionality *joinChild(subnetAddress, validatorData)*:
- *validatorData* contains validator’s info, such as their IP, to allow discovery from other validators of the subnet
- It is implemented using the *writeArbitraryData(in, out, data)* functionality, where:
    - *in*: UTXO(s) spendable by the node that wants to become a validator
    - *out*: a UTXO with collateral BTC locked by *subnetPK*
    - *data*: *validatorData* + *subnetData*

Similarly to createChild, after a joinChild transaction is detected, the btc_monitor extracts the parameters 
encoded in the commit-reveal transaction which contain the validator and subnet data and updates the state of 
the particular subnet. joinChild behaves exactly the same in a single and multi-validator setting. The only difference
is the number of validators that call execute the joinChild command.

## Checkpoint
We model this as a functionality *checkpoint(checkpointHash)*
It is implemented as a single bitcoin transaction with the following inputs and outputs:
    - *in*: UTXO(s) spendable by the *subnetPK* that is submitting the checkpoint
    - *output*: a UTXO with 0 value, containing the OP_RETURN opcode *ipcCheckpointKeyword* *checkpointHash*

The relayer is responsible for periodically submitting checkpoints on behalf of the subnet. the Relayer obtains a 
commitment (implemented as a hash) of the state from the subnet (specifically for Stage 1 from the simulator), it 
creates a bitcoin transaction that includes the IPC checkpoint command keyword and the commitment, and asks the 
validators to sign it. Upon constructing a valid signature, the relayer submits the transaction to the network.

It is the responsibility of the btc_monitor to also detect checkpoint transactions. Upon examining the OP_RETURN outputs 
and detecting the IPC checkpoint keyword, the btc_monitor looks for the subnet whose *subnetPK* verifies
the signature produced on the transaction inputs. After successful signature verification, the btc_monitor is aware
of the checkpoints made for a particular subnet.


## Deposit
This command allows subnet users to deposit funds from their bitcoin wallet to their subnet address (denoted *userAddress*). 
Specifically, they can "lock" an amount of BTC on L1 and "mine" an equal amount of *wrapped BTC* on the L2 subnet.

We model this as a functionality *deposit(subnetAddress, amount, userAddress)*.
It is implemented as a single bitcoin transaction with the following inputs and outputs:
- *in*: UTXO(s), spendable by the user's wallet, with total value the desired *amount* plus the miner's fee.
- *output*: (1) A UTXO of value *V*, locked with the *subnetPK* that corresponds to *subnetAddress*. (2) A UTXO with 0 value, containing the OP_RETURN opcode *ipcDepositKeyword* *userAddress*.
The function is signed and submitted by the user. 

Prior to creating this transaction, the user locally creates a secret key and obtains a *userAddress* for the subnet.

The deposit transaction also contains an IPC command keyword to be able to be detected by the btc_monitor. 
The BTC monitor checks if a subnet exists such that the script pubkey of the deposit transaction corresponds to 
a script pubkey generated using a *subnetAddress*. This is the way that btc_monitor fully identifies a
deposit trnasaction. After fetching all the required parameters, the btc_monitor calls the subnet simulator deposit function.
Since every validator operates both a btc_monitor and a subnet simulator, no modifications to the deposit transaction are needed, 
regardless of whether the system operates in a single or multi-validator setting.

