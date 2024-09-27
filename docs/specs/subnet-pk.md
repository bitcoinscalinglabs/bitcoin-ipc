Please make sure you have read `architecture.md` first, for a definition of modules such as "Relayer" and "Subnet Simulator".

# Identifying a subnet

## subnetPK
The *subnetPK* for an L2 subnet is a *bitcoin script* that can be unlocked by the validators of that subnet.

**Example: Locking funds.**
A bitcoin user that wants to deposit funds from bitcoin to an L2 (lock BTC and mint wrapped BTC) broadcasts a transaction that locks a specific amount of BTC on the bitcoin network with the *subnetPK* of the L2 subnet.

**Implementation.**
The subnetPK is implemented as a Pay-to-Taproot (P2TR) script in all three Stages of the project.

## subnetSig
It is a *bitcoin witness* that can be used to unlock a UTXO locked with *subnetPK*.

**Example: Unlocking funds.**
When an L2 subnet wants to release funds from the L2 back to bitcoin chain (a user withdraws wrapped BTC), the validators of the subnet provide their signatures to create a *subnetSig*, which is the witness that unlocks a UTXO locked with *subnetPK*.

**Implementation.** The subnetSig is implemented as a taproot witness, which unlocks subnetPK using either the key-spend path (in Stages 1-2, and potentially in Stage 3) or the script path (in Stage 3, if we decide to change from the key-spend path). We remark that changing from the key-spend to the script path is a simple change, confined to specific parts of the code.

## Uniqueness of subnetPK
Each `subnetPK` must be unique. If two subnets have the same `subnetPK`, then the validators of one would be able to affect the state of the other. Note that this also applies if the subnets have the same validators.


# Computing subnetPK in Stages 1-2
In Stage 1, subnets have a single validator: the same bitcoin user submits the `createChild()` function and the `joinChild()` function and becomes the single validator.

The *subnetPK* contains the public key of the single validator.
The *subnetSig* uses the key-spend path to unlock the subnetPK script. The single validator can produce such a valid witness using its private key.



# Computing subnetPK in Stage 3
In Stage 3, the bitcoin user that submits `createChild()` does not have to become a validator, and certainly will not control the `subnetPK`.

We have two options:
1. Use the key-spend path of taproot signatures: We use a DKG protocol to generate secret-key shares for all validators and a single public key. The *subnetPK* contains the public key. The *subnetSig* uses the key-spend path to unlock the subnetPK script. For this, validators need to engage into a threshold-signing protocol to compute a valid signature.
2. Use the script path of taproot signatures: The *subnetPK* contains the public keys of all validators and a threshold. The *subnetSig* uses the script path to unlock the subnetPK script. For this, we use a multi-signature protocol that outputs at least a threshold of signatures from the validators.


### An approach for permissioned L2s, using DKG (we will implement this in Stage 3)
- The creator ofthe  subnet specifies an initial set of validators, identified by bitcoin public keys. 
- These validators can use the list of 4 pks to create a MultiSig (req. 3 of 4 sigs) script to lock their collateral. This script in the end locks all collateral.
- When all validators lock, DKG is run.
- The output of DKG is a public key (plus key shares) which determines (when seen as a P2TR script using the key-spend path) determines the *subnetPK*.
- The subnet is bootstrapped. The validators are in a “white list”. This whitelist is encoded in the createSubnet transaction, which is already a commit-reveal. The whitelist is created offline.
- The validators sign a transaction that spends the collaterals locked with the mutlisig script and sends it to the newly created *subnetPK*.

After the initial DKG is completed, we allow validators to join and leave the subnet and change their stake.
- We change subnet_PK when the membership sufficiently changes, while maintaining a *Naming Service* on Bitcoin which maps subnet names to subnet_PKs.
    - Pikachu style
- Rules for joining:
    - Old (Initial) validators have already run DKG
    - New validator calls joinChild() (which locks collateral)
    - Old validators update subnet public key to include new (run DKG again)
    - By default we do not update the white list (it becomes relevant after the initial setup). We can do that on particular use cases.

Observe that this is a hybrid approach. We use a multisig script for the initial set of validators and a P2TR script with a single public key (generated using DKG) afterwards.
This overcomes the problem of validators having to lock their collateral with the subnet's public key before that public key is generated.

<details>
  <summary>Details</summary>
  See meeting agenda of Sep. 18 for more details and remaining open questions.
</details>

### A possible approach for permissionless L2s, using DKG (not explored for now)
The process will be the following (for now in a high level):

- a  bitcoin user submits `createChild()` with `num_validators=n`(among others) as arguments
- when this becomes finalized on the bitcoin chain, users that want to join the subnet submit an `rsvpChild()` transaction. This will be a simple (not taproot commit-reveal) bitcoin transaction
- when `n` `rsvpChild()` transactions get finalized on chain, the Relayer starts a DKG protocol with all `n` validators. After this process:
    - validators locally output shares of `Pubkey`, called `SecretKey_i`. They store them locally and use them whenever the *Relayer* asks them to sign something.
    - the *Relayer* outputs a `PubKey`, which is used to create the *subnetPK* script. The *Relayer* posts this on bitcoin through a transaction `childReady(subnetPK)`.
- when the transaction `childReady(subnetPK)` is finalized, each validator
    - calls (as in Stage 1) `joinChild()`, which will send the collateral to a UTXO controlled by `subnetPK`
    - starts a subnet node and connects to the other validators
- subsequently, when the subnet has to create a `subnetSig` (e.g., for the checkpoint() function), the *Relayer* orchestrates a threshold-signature protocol among the validators of the subnet.
