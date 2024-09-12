Please make sure you have read `architecture.md` first, for a definition of modules such as "Relayer" and "Subnet Simulator".

## Definitions
For an L2 subnet, we define the following.

### subnetPK
It is a collection of bitcoin public keys, one for each subnet validator. The exact representation is an implementation detail — it could be encoded as a list of separate keys, or as a single aggregate key, or both in different places.

Every L2 subnet is associated with a single and unique `subnetPK`. For example, we denote the `subnetPK` of subnet `A` as `subnetPK_A`.

### subnetMultiSig
It is a signature that verifies against `subnetPK`.

### Private keys
The private keys that correspond to `subnetPK_A` are denoted by `SubnetSK_A_i`, for `i` in `[1, n]`, where `n` is the number of validators in subnet `A`.  That is, each validator holds a *share* of  `SubnetSK_A`.

Each `SubnetSK_A_i` is a bitcoin private key and used by the validator when the subnet as a whole needs to interact with bitcoin, e.g., for checkpointing and withdrawing. The Relayer always initiates such processes (for example, see checkpointing above). These keys are not to be confused with the keys that validators already use for communicating with each other, or in the consensus protocol they are running.

## Computing `subnetPK`
In the general case, `subnetPK` is defined when **all validators** of the subnet are known. In Stage 1 specifically, this will be different, as explained below.

**Remark**: Each `subnetPK` must be unique. If two subnets have the same `subnetPK`, then the validators of the one would be able to affect the state of the other. Note that this also applies if the subnets have the same validators.



### Specifically for Stage 1:
In Stage 1, subnets have a single validator. The same bitcoin user submits the `createChild()` function and the `joinChild()` function, hence becoming the single validator.
This user defines the `subnetPK` and can produce valid `subnetMultiSig`.
The process of creating and joining a child subnet is the following:

- create a bitcoin key pair, call the secret and public keys `SubnetSK_A` and `subnetPK_A`, respectively
- store locally the  `SubnetSK_A`
- use `subnetPK_A` when calling `createChild()`
- call `joinChild()`, which will send the collateral to a UTXO controlled by `subnetPK_A`
- start the `subnet_simulator` binary. This reads the locally stored `SubnetSK_A`, and uses it whenever the *Relayer* asks it to create a `subnetMultiSig` (e.g., sign a checkpoint message).

### In Stage 3:
In Stage 3, the bitcoin user that submits `createChild()` does not have to become a validator, and certainly will not control the `subnetPK`. The process will be the following (for now in a high level):

- a  bitcoin user submits `createChild()` with  `name=A`  and `num_validators=n`(among others) as arguments
- when this becomes finalized on the bitcoin chain, users that want to join subnet `A`  submit an `rsvpChild(A)` transaction. This will be a simple (not taproot commit-reveal) bitcoin transaction
- when `n` `rsvpChild(A)` transactions get finalized (or simply appear?) on chain, the Relayer starts an interactive, off-chain protocol with all `n` validators to compute the  `subnetPK_A`.Through this process:
    - validators locally output shares of `SubnetSK_A`, called `SubnetSK_A_i` . They store  them locally and use them whenever the *Relayer* asks them to sign something (this will be through another multiparty protocol, which will output a valid `subnetMultiSig`)
    - the *Relayer* outputs the `subnetPK_A`. The *Relayer* posts this on bitcoin through a transaction `childReady(A, subnetPK_A)` .
- when the transaction `childReady(A, subnetPK_A)` is finalized, each validator
    - calls (as in Stage 1) `joinChild()`, which will send the collateral to a UTXO controlled by `subnetPK_A`
    - starts a subnet node and connects to the other validators
- subsequently, when the subnet has to create a `subnetMultiSig` (e.g., for the checkpoint() function), we are planning to have the *Relayer* orchestrate a multi-signature protocol among the validators of the subnet.