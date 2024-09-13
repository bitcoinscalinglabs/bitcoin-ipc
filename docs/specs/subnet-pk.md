Please make sure you have read `architecture.md` first, for a definition of modules such as "Relayer" and "Subnet Simulator".

# Identifying a subnet

## subnetPK
The *subnetPK* for an L2 subnet is a *bitcoin script* that can be unlocked by the validators of that subnet.

**Example: Locking funds.**
A bitcoin user that wants to deposit funds from bitcoin to an L2 (lock BTC and mint wrapped BTC) broadcasts a transaction that locks a specific amount of BTC on the bitcoin network with the *subnetPK* of the L2 subnet.

**Implementation.**
The subnetPK is implemented as a Pay-to-Taproot (P2TR) script.

## subnetSig
It is *bitcoin witness* that can be used to unlock a UTXO locked with *subnetPK*.

**Example: Unlocking funds.**
When an L2 subnet wants to release funds from the L2 back to bitcoin chain (a user withdraws wrapped BTC), the validators of the subnet provide their signatures to create a *subnetSig*, which is the witness that unlocks a UTXO locked with *subnetPK*.

**Implementation.** The subnetSig is implemented as a taproot witness. 

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


### Using the DKG approach
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
