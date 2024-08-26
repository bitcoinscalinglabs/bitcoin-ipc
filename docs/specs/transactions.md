## Attach arbitrary data to bitcoin transactions
This will be done using the taproot script-path.

We model this as a functionality **txWithArbitraryData(in, out, data**), where *in* and *out* are the input and output UTXOS, respectively, and *data* some arbitrary data. The functionality does the following:
- Create a script containing *data*.
- Create a first Bitcoin transaction that spends the UTXO(s) *in* and creates an output UTXO *temp* that contains a hash of the script.
- Create a second bitcoin transaction that spends *temp* by revealing the content of the script as the *witness,* and outputs *out* as the output UTXO. (Observe that *data* is submitted with a lower transaction fee as witness)
- These two bitcoin transactions are submitted to Bitcoin by the same entity.

## Create child subnet
We model this as a functionality *createChild(subnetPK, subnetData)*:

- *subnetData* contains the following data:
    - A known tag that this transaction is about creating a new IPC Subnet.
    - The subnet name
    - Possibly more arbitrary data
- The functionality will be implemented using the *txWithArbitraryData(in, out, data)* functionality, where:
    - *in*: UTXO(s) spendable by the wallet that submits *createChild()*
    - *out*: a UTXO with some amount locked by *subnetPK*, which will be used to pay the transactions fees in later stages.
    - *data*: *subnetData*

Here is the flow of a subnet creation.
![Create Subnet](../diagrams/create-subnet.png)


## Join subnet
We model this as a functionality *joinChild(subnetPK, validatorData)*:
- *validatorData* contains validator’s info, such as their IP, to allow discovery from other validators of the subnet
- The transaction will be submitted using the *txWithArbitraryData(in, out, data)* functionality, where:
    - *in*: UTXO(s) spendable by the node that wants to become a validator
    - *out*: a UTXO with collateral BTC locked by *subnetPK*
    - *data*: *validatorData*