## Dynamic participation (committee changes and rotations)

This document describes how validator participation changes over time, how those changes are recorded from Bitcoin transactions, and how the active committee is advanced via checkpoint-driven committee rotation.

The model is **event-driven**: once a transaction is considered finalized on Bitcoin, it is parsed as an IPC message and applied to protocol state.

### High-level model

- **Stake-change requests** (join/stake/unstake) are recorded on Bitcoin and stored in the DB as `StakeChangeRequest` objects. They update a *proposed* next committee (the `waiting_committee`) and advance a **configuration counter**.
- A **checkpoint** transaction can optionally **rotate** the subnet to a new committee. Rotation is what makes pending stake-change requests *effective*.
- Conceptually, the protocol maintains both:
  - committee snapshots indexed by `committee_number`, and
  - stake-change requests indexed by `configuration_number`.

### State and counters

Notation used below (pseudocode):

```text
X <- Y            assign
X <- NULL         no value / does not exist
X <- Some(Y)      optional value is set
X == Y            equality test
X != Y            inequality test
X += k            increment by k
```

#### `SubnetState`

- `committee_number: u64`
  - An **activated committee index**.
  - Advances only when a checkpoint rotates to a new committee.
- `committee: SubnetCommittee`
  - The currently active committee.
- `waiting_committee: Option<SubnetCommittee>`
  - A proposed next committee that includes pending stake changes not yet made effective by a checkpoint.
- `last_checkpoint_number: Option<u64>`
  - The latest checkpoint number observed for this subnet.
- `killed: SubnetKillState`
  - `NotKilled`, `ToBeKilled`, or `Killed { parent_height }`.
  - Kill requests only mark `ToBeKilled`; a kill **checkpoint** sets `Killed { parent_height }`.

#### `SubnetCommittee`

- `configuration_number: u64`
  - A **stake-change nonce** for the committee configuration.
  - Advances when the committee content changes due to stake-change requests:
    - join: `+2` (metadata + deposit)
    - stake/unstake or removal: `+1`

#### `StakeChangeRequest`

Each stake-change request contains:
- `configuration_number`: the nonce assigned when the request is recorded
- `committee_after_change`: the committee that would be active if this request (and its predecessors) were applied
- `block_height`/`block_hash`/`txid`: where the request was recorded on Bitcoin
- `checkpoint_block_height`/`checkpoint_block_hash`: filled in when a checkpoint confirms/applies the request

#### `SubnetCheckpoint`

Key fields:
- `signed_committee_number`: committee that signed the checkpoint
- `next_committee_number`: committee number after the checkpoint (may be the same if no rotation)
- `next_configuration_number`: configuration number after the checkpoint (the stake-change nonce applied by this checkpoint)
- `block_height`: Bitcoin height where the checkpoint tx was included
- `is_kill_checkpoint`: whether this checkpoint kills the subnet

### Initialization

#### CreateSubnet (subnet not bootstrapped yet)

Upon `CreateSubnet` finalized on Bitcoin at height `h`:

```text
SubnetGenesisInfo <- create_subnet_params
SubnetState <- NULL
```

#### Bootstrapping (creating the first active committee)

Bootstrapping happens when enough validators join in the pre-bootstrap phase.

Upon a `JoinSubnet` finalized on Bitcoin at height `h` that causes the subnet to bootstrap:

```text
SubnetState.committee_number <- 1
SubnetState.committee.configuration_number <- 0
SubnetState.waiting_committee <- NULL
SubnetState.last_checkpoint_number <- NULL
```

### Event-driven updates

Below are the protocol state transitions. Each transition is triggered **upon a transaction finalized on Bitcoin at height `h`**.

#### Upon `JoinSubnet` finalized on Bitcoin at height `h`

There are two cases:

- **Pre-bootstrap** (no `SubnetState` yet):

```text
SubnetState == NULL
SubnetGenesisInfo.genesis_validators.append(new_validator)

if SubnetGenesisInfo.enough_to_bootstrap():
    // create SubnetState as in "Bootstrapping"
```

- **Post-bootstrap** (a `SubnetState` exists):

```text
SubnetState != NULL

next_committee <- SubnetState.waiting_committee OR SubnetState.committee
next_committee.join_new_validator(new_validator)
    // implies: next_committee.configuration_number += 2

SubnetState.waiting_committee <- Some(next_committee)

// record two stake-change requests at the current Bitcoin height h
StakeChangeRequest(N)   <- Join(pubkey)
StakeChangeRequest(N+1) <- Deposit(amount)
```

#### Upon `StakeCollateral` finalized on Bitcoin at height `h`

```text
next_committee <- SubnetState.latest_committee()
next_committee.modify_validator(updated_validator)
    // implies: next_committee.configuration_number += 1

SubnetState.waiting_committee <- Some(next_committee)

StakeChangeRequest(N) <- Deposit(amount)
```

#### Upon `UnstakeCollateral` finalized on Bitcoin at height `h`

```text
next_committee <- SubnetState.latest_committee()

if new_power == 0 OR new_collateral < min_validator_stake:
    next_committee.remove_validator(pubkey)
else:
    next_committee.modify_validator(updated_validator)

// either path implies: next_committee.configuration_number += 1
SubnetState.waiting_committee <- Some(next_committee)

StakeChangeRequest(N) <- Withdraw(amount)
```

#### Upon `CheckpointSubnet` finalized on Bitcoin at height `h`

```text
// input carried by the checkpoint tx:
next_committee_configuration_number

if no_rotation:
    SubnetCheckpoint.next_committee_number <- SubnetState.committee_number
    SubnetCheckpoint.next_configuration_number <- SubnetState.committee.configuration_number
    // active committee unchanged
```

- Otherwise, it looks up `StakeChangeRequest(subnet_id, next_committee_configuration_number)`:

```text
stake_change <- StakeChangeRequest(next_committee_configuration_number)
next_committee <- stake_change.committee_after_change

SubnetCheckpoint.signed_committee_number <- SubnetState.committee_number
SubnetCheckpoint.next_committee_number <- SubnetState.committee_number + 1
SubnetCheckpoint.next_configuration_number <- stake_change.configuration_number

SubnetState.rotate_to_committee(next_committee)
    // implies: SubnetState.committee_number += 1
    // and makes next_committee effective

confirm StakeChangeRequest(k) for all k <= next_committee_configuration_number
    // fills StakeChangeRequest.checkpoint_block_height/hash
```

##### Kill checkpoints

If `is_kill_checkpoint` is true, the checkpoint additionally sets:

```text
SubnetState.killed <- Killed { parent_height: h }
```

#### Upon `KillSubnet` finalized on Bitcoin at height `h`

```text
KillRequest.append(validator_vote)

if voting_power_reaches_threshold:
    SubnetState.killed <- ToBeKilled
    marked_for_kill_checkpoint_number <- SubnetState.last_checkpoint_number (if any)

// SubnetState becomes Killed only on a kill checkpoint.
```

### How to determine the active committee at a given Bitcoin height

Under the current model, committee changes become effective only at checkpoints. Therefore, to find the committee active at Bitcoin height `H`:

```text
committee_number <- 1
for checkpoint in SubnetCheckpoint in increasing checkpoint_number:
    if checkpoint.block_height <= H:
        committee_number <- checkpoint.next_committee_number

active_committee <- CommitteeSnapshot(committee_number)
```

This is the same logic used in reward eligibility computations that are based on effective committees over a height interval.

### Key takeaways and invariants

- `configuration_number` and `committee_number` are related but **not equal**:
  - `configuration_number` can advance many times between checkpoints.
  - `committee_number` advances at most once per checkpoint (only on rotation).
- Stake-change requests are recorded as soon as their Bitcoin tx is finalized; they become effective only when a checkpoint applies them.
- The authoritative effective committee history is the sequence of `SubnetCheckpoint` records plus the committee snapshots identified by their `committee_number`.
