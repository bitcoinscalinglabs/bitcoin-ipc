# Summary

Create a PoC deployment of IPC over Bitcoin.

The deliverable is a demo, along with all the relevant code and documentation, that demonstrates the following 6 steps:

1. Start a Bitcoin testnet (L1) and two IPC subnets (L2) *A, B*
2. Take a checkpoint from each subnet to Bitcoin
3. Deposit funds from Bitcoin to an account on *A*
4. Transfer the funds from multiple accounts on *A* to multiple accounts on *B*
5. Withdraw the funds from that account on *B* to Bitcoin
6. Close down *A* and *B*

## Assumptions:

- All the subnet validators also run their own Bitcoin full-node

## Specific requirements and details for each step

- **Step 1: Start a subnet *A* on top of Bitcoin.**
    - Anyone should be able to initiate the instantiation of a subnet by submitting a single transaction on Bitcoin.
- **Step 2: Checkpoint from Subnet *A* to Bitcoin**
    - Submit snapshots of ***A***’s state on Bitcoin, signed by the ***A***’s key. This signature of ***A*** serves as the verification that this snapshot is valid.
- **Step 3: Deposit funds from Bitcoin to *A***
    - We want to enable some wallet ***x*** on Bitcoin to lock some amount ***v*** of BTC and mint “Wrapped BTC” in *A,* towards the account ***A.y***.
    - This is done in two steps:
        - The owner of ***x*** submits Bitcoin *tx=Deposit(A, y, v)*
        - The flow of verifying the Bitcoin transaction and minting the tokens on ***A*** can be either of the following:
            - Option 1:
                - Validators of ***A*** find the *Deposit* transaction from their respective Bitcoin full nodes
                - Each of them then calls *the MintDeposited(y, v)* function on a smart contract on ***A***.
                - Once enough of the validators call the *MintDeposited* function, the tokens are minted towards the subnet address ***y***
            - Option 2:
                - The *MintDeposited* function also accepts an SPV proof and can be called by anyone. The caller has to provide:
                    - A proof that the *Deposit()* transaction is included in a Bitcoin block
                    - The Bitcoin block that includes the *Deposit()* transaction
                    - A chain of headers from the genesis block up to that block
                - The smart contract can then verify this proof, given that it knows the genesis block from a trusted source, and can then mint the tokens to the address ***y***.
            - **Design choice**: We are taking Option 1, because we assume in this project that L2 validators run bitcoin full nodes.
- **Step 4: Withdraw from *A* to Bitcoin.**
    - The inverse logic from Deposit
    - Some account owner on *A* submits a Withdraw(bitcoinDestinationAddress, amount)* to ***A***
    - When *Withdraw()* becomes finalized, the validators of ***A** create and sign a ReleaseWithdrawn(bitcoinDestinationAddress, amount)* Bitcoin transaction that sends the specified amount of BTC to the *bitcoinDestinationAddress*.
- **Step 5: Transfer funds from subnet *A* to subnet *B***
    - Users should be able to transfer their Wrapped BTC tokens between the different subnets.
    - The transfer of value should also be reflected on Bitcoin, by transferring the same amount from one subnet’s wallet to the other’s.
    - The transfer should also account for fees spent on Bitcoin, reducing the amount that is received on the destination subnet.
    - Transfers between subnets should be batched to save fees.
- **Step 6: Remove Subnet *A***
    - Remove the subnet by returning the collateral back to the validators.



# Stages
The project is divided into three stages, each stage has the same deliverable, initial stages make more abstractions and later stages replace them with an actual implementation.

## Stage 1

In this stage the IPC subnets are simulated with a simple process acting as the Simulator

The goal of this stage is to solve Bitcoin-side questions, such as

- write subnet information on Bitcoin, lock collateral, start the subnet
- create UTXO that belong to the subnet, use them for checkpointing
- transfer funds between Bitcoin wallets and UTXOs that belong to subnets
- transfer funds between UTXOs that belong to different subnets

It contains the following deliverables:

### Step 1: Create child subnet *A*

- Definition for a *Subnet Multisig*: A Bitcoin script that requires the signature from some specified keys in order to spend a UTXO. We refer to this set of keys as *subnetPK*. For the first stage, only a signature from one key will be required. This key is controlled by the the Simulator for subnet *A*.
>  **See `subnet-pk.md` for a more precise definition of `subnetPK`.**

- Implement functionality *writeArbitraryData()* to attach arbitrary data on a Bitcoin transaction.
>  **See `transactions.md` for the definition of writeArbitraryData().**

- Definitions for the following Bitcoin transactions:
    - *createChild()* can be created by anyone who wants to create a subnet (doesn’t have to be a validator on the new subnet).
    - *createChild()* can be used by any bitcoin node that wants to become a validator on the subnet indicated by *subnetPK*
>  **See `transactions.md` for the definition of createChild() and joinChild().**
        
- The following tools
    - Script to start a local Bitcoin testnet with *bitcoind* ~~(or connects to an existing full node, **TBD**)~~
    - Script for the initialization of a subnet
        - Creates a new Bitcoin public key (*subnetPK)* for the subnet
        - Submits a *createChild(subnetPK, subnetData)* transaction on Bitcoin
    - Script for joining the subnet that submits a *joinSubnet(subnetPK, validatorData)* transaction on Bitcoin
    - An off-chain service that waits for the joinSubnet() transaction to be included in the chain and when it does, it communicates with the Simulator to signal the subnet creation.
    - For the Proof-of-Concept, all those scripts will be run from a Bitcoin full node that wants to create a new subnet and become the sole validator to it.

### Step 2: Checkpoint from *A* to Bitcoin
- Definitions for the following Bitcoin transactions:
    - A transaction *Checkpoint(block_hash)* which is signed by *subnetPK* and contains the latest finalized *block_hash* from subnet *A* in the **OP_RETURN** UTXO. This transaction just pays the network fees, and returns the change back to the Subnet Multisig.
- The following tools (the script act as relayer between *A* and Bitcoin)
    - A script that initiates checkpointing, acting as a relayer between ***A*** and Bitcoin:
        - The script gets an arbitrary *block_hash* from the Simulator.
        - It then creates *Checkpoint(block_hash)* transaction, signs it (using *subnetPK*) and submits it to Bitcoin.

### Step 3: Deposit funds from Bitcoin to *A*
- Definitions of Bitcoin transactions:
    - A transaction *Deposit(subnetPK, amount, subnetDestinationAddress)* created by any Bitcoin wallet, that locks an amount of BTC with *subnetPK* and contains the destination address (of ***y***), to receive the minted tokens, in the **OP_RETURN** UTXO.
- The following tools:
    - A script that creates a *Deposit* transaction on Bitcoin
    - Off-chain service executed on the validators’ machines and detects a *Deposit* transaction on Bitcoin, relevant to the subnet ***A***.
        - it parses the Bitcoin transaction to get the destination address from the **OP_RETURN** UTXO
        - it calls the Simulator which simply increments an internal balance counter

### Step 4: Transfer funds from subnet *A* to subnet *B* 
- Definition of a Bitcoin *Propagate(A, B, Transfers[])* transaction:
    - The transaction will be submitted using the *writeArbitraryData(in, out, data)* functionality, where:
        - *in*: **UTXO(s) spendable by *A*
        - *out*: **a UTXO locked with the *subnetPK* of *B,* with the total amount to be transferred
        - *data*: **it contains the destination subnet and information about the transfers to be made: the amounts and the destination addresses.
- The following tools:
    - Repeat Step 1 to create a second subnet B (In this stage instantiated by a second simulator script).
    - Off-chain service that waits for Simulator A to output multiple *Transfer()* (from A to B) transactions
        - It then creates the *Propagate(A, B, Transfers[])* transaction transferring BTC from ***A*** (using the *subnetPK of A*) to ***B***.
    - Another off-chain service that looks for *Propagate* transactions on Bitcoin:
        - When it detects such transaction, it calls a function on the Simulator of the destination subnet that mints the specified amount of tokens to the destination addresses.

### Step 5: Withdraw from B to Bitcoin
- Definitions of Bitcoin transactions:
    - A transaction *ReleaseWithdrawn(bitcoinDestinationAddress, amount)* signed by *B*’s *subnetPK* releasing *amount* of BTC to the *bitcoinDestinationAddress*. **
- The following tools:
    - Off-chain service that waits for Simulator B to output a Withdraw*(bitcoinDestinationAddress, amount)* transaction
        - When it detects such transaction, it creates a *ReleaseWithdrawn(bitcoinDestinationAddress, amount),* signs it with S*ubnetPK* of B, and submits it to Bitcoin.

### Step 6: Remove subnets
- Definitions of Bitcoin transactions:
    - A transaction *removeChild(A)* that returns the collateral to the validators, essentially by reversing all the *joinChild* transactions related to this subnet. This transaction also contains a tag in **OP_RETURN** to signify that it removes a subnet. The transaction is signed by the subnet ***A***’s *subnetPK*.
- The following tools:
    - Off-chain service that waits for a command from the Simulator to remove the subnet.
        - Upon receiving this command, it finds the *joinChild* transaction on Bitcoin releases the collateral back to sender by creating a *removeChild* transaction as defined above.
        - → The btc_monitor might be the right place to implement this?


## Stage 2
In this stage the IPC subnets are instantiated with a Fendermint network with a single validator. The goal is to better understand the IPC codebase and use an actual IPC subnet, albeit still with a single validator.

This stage does not include changes on the Bitcoin transactions. It may **require collaboration with the IPC team.** Some steps can be performed by the IPC team.

It contains the following deliverables, incremental on the previous stage:

### Step 1: Create child subnet *A*
- Extend the off-chain service that waits for the *joinSubnet()* transaction to start a new Fendermint node and create an actual subnet.

### Step 2: Checkpoint from *A* to Bitcoin
- The script that initiates the checkpointing will obtain the latest *block_hash* from the Fendermint subnet. The *Checkpoint* transaction will be signed by the validator’s Bitcoin key.

### Step 3: Deposit funds from Bitcoin to *A*
- An ERC20 token (Wrapped BTC) is deployed on subnet ***A*** and only the single validator can mint tokens towards an address.
    - **Open Question**: Does Fendermint come with an implemented application?
- The off-chain service calls the *mint()* function on the Fendermint ERC20 token instead of the Simulator.

### Step 4: Transfer funds from subnet *A* to subnet *B*
- The transfer process begins by calling *Transfer(amount, B, destinationAddress)* on the smart contract of the Wrapped BTC token. This function burns the specified amount of Wrapped BTC on this subnet.
- An off-chain service running on the machine of ***A***’s validator detects this transaction and creates a *Propagate* Bitcoin transaction.
- Another off-chain service running on the machine of ***B***’s validator detects the *Propagate* Bitcoin transaction and calls the *mint()* function of the token’s contact on subnet ***B***.

### Step 5: Withdraw from B to Bitcoin
- The off-chain service is running on the validator’s machine. It monitors Fendermint and detects *Withdraw(amount, bitcoinAddress)* transactions on the ERC20 token (Wrapped BTC).
    - The *Withdraw(amount, bitcoinAddress)* transaction will burn the specified amount of tokens.
    - When the *Withdraw* transaction is observed, the script will create and submit a *ReleaseWithdrawn* transaction on Bitcoin.

### Step 6: Remove subnets
- Script which runs on the validators’ machines that initiates the removal of *A*. For this, the script needs to do the following
    - Detect a *ToRemove* transaction on the subnet.
    - Retrieve all *joinSubnet* transactions related to the subnet and extract the UTXOs that hold collaterals of validators.
    - Create a *removeChild* transaction and submit it to Bitcoin.


## Stage 3
In this stage the IPC subnets are instantiated with a Fendermint network with multiple validators, stake, and dynamic participation.

The goals of this stage are to
- generalize the *Subnet Multisig*, so that it can support dynamic and stake-based validators
- Explore how validators find and connect to each other on A

It contains the following deliverables, incremental on the previous stage:

### Step 1: Create child subnet *A*
- We will explore the following options for creating a *Subnet Multisig* that is controlled from all the validators:
    1. Use a script  that requires M out of N validators. Each validator has their own key, and each signature can also have a weight based on the stake of the validator.
    2. Setup an off-chain MPC protocol between the validators to create an aggregated key. When the subnet creates a new Bitcoin transaction, it will require the signatures from multiple validators, creating an aggregated Schnorr signature. References: [FROST](https://eprint.iacr.org/2020/852), [ROAST](https://eprint.iacr.org/2022/550), [WSTS](https://trust-machines.github.io/wsts/wsts.pdf).
- The process of creating a subnet will need to be adjusted depending on the design decision from the point above. Each validator that wants to join the network will also need to provide some data on-chain for other validators to find them.

### Step 2: Checkpoint from *A* to Bitcoin
- Depending on the design choice from the previous step, the creation and signing of the *Checkpoint* transaction will need to be adjusted to accommodate for multiple validators.

### Step 3: Deposit funds from Bitcoin to *A*
- Extend the ERC20 token. It should check that enough of the validators have called the *mint()* function before minting the tokens.

### Step 4: Transfer funds from subnet *A* to subnet *B*
- Depending on the design choice from **Step 1**, the creation and signing of the *Propagate* transaction needs to be adjusted to accommodate for multiple validators.
- The token’s contract on the destination chain also has to be adapted to wait for multiple validators to call *mint()* before minting the tokens.

### Step 5: Withdraw from B to Bitcoin
- Depending on the design choice from **Step 1**, the creation and signing of the *ReleaseWithdrawn* transaction needs to be adjusted to accommodate for multiple validators.

### Step 6: Remove subnets
- Depending on the design choice from **Step 1**, the creation and signing of the *RemoveChild* transaction needs to be adjusted to accommodate for signatures from multiple validators.~