use super::*;
use crate::collateral::process_join_ue;
use crate::ipc_lib::{IpcBatchTransferMsg, IpcValidate};
use crate::slash::SlashSignatureEntry;
use crate::test_utils::*;
use bitcoin::{hashes::Hash, key::Parity, Amount, BlockHash, OutPoint, Txid, XOnlyPublicKey};
use std::str::FromStr;

fn create_test_rootnet_message(
    subnet_id: SubnetId,
    nonce: u64,
    block_height: u64,
) -> RootnetMessage {
    let dummy_txid = create_rand_txid();
    let dummy_block_hash = create_rand_blockhash();

    let msg = IpcFundSubnetMsg {
        subnet_id,
        amount: Amount::from_sat(1_000_000),
        address: create_rand_eth_addr(),
    };

    RootnetMessage::FundSubnet {
        msg,
        block_height,
        block_hash: dummy_block_hash,
        nonce,
        txid: dummy_txid,
    }
}

fn test_ue_collateral(subnet_id: SubnetId) -> UeCollateral {
    UeCollateral {
        subnet_id,
        pubkey: generate_xonly_pubkeys(1)[0],
        outpoint: bitcoin::OutPoint::new(create_rand_txid(), 1),
        value: Amount::from_sat(100_000),
        timeout: 1_000,
        pk_eots: generate_xonly_pubkeys(1)[0],
        committee_number: 0,
        ip: "127.0.0.1:8080".parse().unwrap(),
        backup_address: Address::from_str("bcrt1q3fznspr3e02artm9df7tk827a2xhny2m4zzr6n")
            .unwrap(),
        join_txid: create_rand_txid(),
        spent: false,
    }
}

/// The multisig of the current configuration and of the previous one name the subnet
#[test]
fn test_subnet_by_current_or_previous_multisig() {
    let db = create_test_db();
    let mut subnet = generate_subnet(3);
    let previous = subnet.committee.validators[..2]
        .to_vec()
        .to_committee(&subnet.id, 1);
    let unrelated = subnet.committee.validators[..1]
        .to_vec()
        .to_committee(&subnet.id, 0);
    subnet.committee_number = 2;

    let mut txn = db.write_txn().unwrap();
    db.save_subnet_state(&mut txn, subnet.id, &subnet).unwrap();
    db.save_committee(&mut txn, subnet.id, 1, &previous)
        .unwrap();
    txn.commit().unwrap();

    for committee in [&subnet.committee, &previous] {
        let found = db
            .get_subnet_by_multisig_address(&committee.multisig_address)
            .unwrap()
            .expect("the subnet of the multisig");
        assert_eq!(found.id, subnet.id);
        assert_eq!(found.committee_number, 2);
    }
    assert!(db
        .get_subnet_by_multisig_address(&unrelated.multisig_address)
        .unwrap()
        .is_none());
}

#[test]
fn test_ue_collaterals_round_trip_and_spend() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    let other_subnet = generate_subnet_id();
    let a = test_ue_collateral(subnet_id);
    let b = test_ue_collateral(subnet_id);
    let c = test_ue_collateral(other_subnet);

    let mut txn = db.write_txn().unwrap();
    for collateral in [&a, &b, &c] {
        db.add_ue_collateral(&mut txn, collateral).unwrap();
    }
    txn.commit().unwrap();

    let mut listed = db.get_ue_collaterals(subnet_id).unwrap();
    listed.sort_by_key(|c| c.join_txid);
    let mut expected = vec![a.clone(), b.clone()];
    expected.sort_by_key(|c| c.join_txid);
    assert_eq!(listed, expected);
    assert_eq!(
        db.get_ue_collaterals(other_subnet).unwrap(),
        vec![c.clone()]
    );
    assert_eq!(
        db.get_ue_collateral(other_subnet, a.join_txid, &a.pubkey)
            .unwrap(),
        None
    );

    // The outpoint index finds a record of any subnet
    let mut d = test_ue_collateral(subnet_id);
    d.timeout = 500;
    let mut txn = db.write_txn().unwrap();
    db.add_ue_collateral(&mut txn, &d).unwrap();
    txn.commit().unwrap();
    for record in [&a, &b, &c, &d] {
        assert_eq!(
            db.get_ue_collateral_by_outpoint(&record.outpoint).unwrap(),
            Some(record.clone())
        );
    }
    let unknown = bitcoin::OutPoint::new(create_rand_txid(), 0);
    assert_eq!(db.get_ue_collateral_by_outpoint(&unknown).unwrap(), None);

    // A spend is recorded by outpoint, in the record and the index; a deleted record
    // leaves the index
    let mut txn = db.write_txn().unwrap();
    db.set_ue_collateral_spent(&mut txn, &a.outpoint).unwrap();
    assert!(db.set_ue_collateral_spent(&mut txn, &unknown).is_err());
    db.delete_ue_collateral(&mut txn, subnet_id, d.join_txid, &d.pubkey)
        .unwrap();
    txn.commit().unwrap();
    assert!(
        db.get_ue_collateral_by_outpoint(&a.outpoint)
            .unwrap()
            .unwrap()
            .spent
    );
    assert_eq!(db.get_ue_collateral_by_outpoint(&d.outpoint).unwrap(), None);
    assert_eq!(
        db.get_ue_collateral(subnet_id, d.join_txid, &d.pubkey)
            .unwrap(),
        None
    );
    assert!(
        db.get_ue_collateral(subnet_id, a.join_txid, &a.pubkey)
            .unwrap()
            .unwrap()
            .spent
    );
    assert!(
        !db.get_ue_collateral(subnet_id, b.join_txid, &b.pubkey)
            .unwrap()
            .unwrap()
            .spent
    );
}

#[test]
fn test_ue_collaterals_of_one_reveal_are_kept_apart_by_key() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    let a = test_ue_collateral(subnet_id);
    let mut b = test_ue_collateral(subnet_id);
    b.join_txid = a.join_txid;
    b.timeout = a.timeout;

    let mut txn = db.write_txn().unwrap();
    db.add_ue_collateral(&mut txn, &a).unwrap();
    db.add_ue_collateral(&mut txn, &b).unwrap();
    txn.commit().unwrap();

    assert_eq!(
        db.get_ue_collateral(subnet_id, a.join_txid, &a.pubkey)
            .unwrap(),
        Some(a.clone())
    );
    assert_eq!(
        db.get_ue_collateral(subnet_id, a.join_txid, &b.pubkey)
            .unwrap(),
        Some(b.clone())
    );
    assert_eq!(db.get_ue_collaterals(subnet_id).unwrap().len(), 2);

    // Spending one leaves the other unspent; deleting it leaves the other stored
    let mut txn = db.write_txn().unwrap();
    db.set_ue_collateral_spent(&mut txn, &a.outpoint).unwrap();
    txn.commit().unwrap();
    assert!(
        !db.get_ue_collateral(subnet_id, b.join_txid, &b.pubkey)
            .unwrap()
            .unwrap()
            .spent
    );
    let mut txn = db.write_txn().unwrap();
    db.delete_ue_collateral(&mut txn, subnet_id, b.join_txid, &b.pubkey)
        .unwrap();
    txn.commit().unwrap();
    let stored = db.get_ue_collaterals(subnet_id).unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].pubkey == a.pubkey && stored[0].spent);
}

#[test]
#[test_retry::retry(3)]
fn test_rootnet_message_nonce() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    let block_height = 100;

    let ro_txn = db.env.read_txn().unwrap();

    // Verify no messages exist initially
    assert_eq!(
        db.get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
            .unwrap(),
        None
    );
    drop(ro_txn);
    // Check next nonce is 0
    assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), 0);

    // Add a message with nonce 0
    let mut txn = db.write_txn().unwrap();
    let msg1 = create_test_rootnet_message(subnet_id, 0, block_height);
    db.add_rootnet_msg(&mut txn, subnet_id, msg1.clone())
        .unwrap();
    txn.commit().unwrap();

    // Check last nonce is now 0
    let ro_txn = db.env.read_txn().unwrap();
    let result = db
        .get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
        .unwrap();
    assert_eq!(result, Some(0));
    drop(ro_txn);

    // Check next nonce is 1
    let next_nonce = db.get_next_rootnet_msg_nonce(subnet_id).unwrap();
    assert_eq!(next_nonce, 1);

    // Add a message with nonce 1
    let mut txn = db.write_txn().unwrap();
    let msg2 = create_test_rootnet_message(subnet_id, 1, block_height);
    db.add_rootnet_msg(&mut txn, subnet_id, msg2.clone())
        .unwrap();
    txn.commit().unwrap();

    // Add a message with nonce 2 at block height 101
    let mut txn = db.write_txn().unwrap();
    let msg2 = create_test_rootnet_message(subnet_id, 2, block_height + 1);
    db.add_rootnet_msg(&mut txn, subnet_id, msg2.clone())
        .unwrap();
    txn.commit().unwrap();

    // Check last nonce is now 2
    let ro_txn = db.env.read_txn().unwrap();
    assert_eq!(
        db.get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
            .unwrap(),
        Some(2)
    );
    drop(ro_txn);

    // Check next nonce is 3
    assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), 3);

    // Verify we can retrieve messages by nonce
    let retrieved_msg0 = db.get_rootnet_msg(subnet_id, 0).unwrap().unwrap();
    match &retrieved_msg0 {
        RootnetMessage::FundSubnet {
            nonce,
            block_height,
            ..
        } => {
            assert_eq!(*nonce, 0);
            assert_eq!(*block_height, *block_height);
        }
        _ => panic!("Expected FundSubnet variant"),
    }

    // Check get_all_rootnet_msgs returns both messages
    assert_eq!(db.get_all_rootnet_msgs(subnet_id).unwrap().len(), 3);

    // Check get_rootnet_msgs_by_height returns only messages at specified height
    assert_eq!(
        db.get_rootnet_msgs_by_height(subnet_id, block_height)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        db.get_rootnet_msgs_by_height(subnet_id, block_height + 1)
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[test_retry::retry(3)]
fn test_rootnet_message_nonce_many_messages() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    // Total number of messages
    let total_message_count = 100;

    // Number of messages per block
    let messages_per_block = 10;

    // Calculate number of blocks needed
    let num_blocks = total_message_count / messages_per_block;

    // Add all messages
    let mut nonce = 0;

    for block in 0..num_blocks {
        let block_height = 1000 + block as u64;
        let mut txn = db.write_txn().unwrap();

        // Add messages_per_block messages to this block
        for _ in 0..messages_per_block {
            let msg = create_test_rootnet_message(subnet_id, nonce, block_height);
            db.add_rootnet_msg(&mut txn, subnet_id, msg).unwrap();
            nonce += 1;
        }

        txn.commit().unwrap();

        // check next nonce
        assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), nonce);
    }

    // Check the last nonce is total_message_count - 1
    let ro_txn = db.env.read_txn().unwrap();
    let last_nonce = db
        .get_last_rootnet_msg_nonce_txn(&ro_txn, subnet_id)
        .unwrap();
    assert_eq!(last_nonce, Some((total_message_count - 1) as u64));
    drop(ro_txn);

    // Check the next nonce is total_message_count
    let next_nonce = db.get_next_rootnet_msg_nonce(subnet_id).unwrap();
    assert_eq!(next_nonce, total_message_count as u64);
    // Check total message count
    let all_msgs = db.get_all_rootnet_msgs(subnet_id).unwrap();
    assert_eq!(all_msgs.len(), total_message_count);

    // Check messages per block for a specific block
    let specific_block = 1005; // should be the 6th block
    let block_msgs = db
        .get_rootnet_msgs_by_height(subnet_id, specific_block)
        .unwrap();
    assert_eq!(block_msgs.len(), messages_per_block);

    // Verify nonces for messages in the specific block
    let expected_first_nonce = messages_per_block * 5; // for block 1005 (0-indexed from 1000)
    let nonces: Vec<u64> = block_msgs.iter().map(|msg| msg.nonce()).collect();

    for (i, &nonce) in nonces.iter().enumerate() {
        assert_eq!(nonce, expected_first_nonce as u64 + i as u64);
    }
}

#[test]
fn test_subnet_committee_join_validator() {
    // Setup: Create a subnet ID and initial committee
    let subnet_id = generate_subnet_id();

    // Create initial validators
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
    let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
    let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

    let validator1 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 10,
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    let validator2 = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 15,
        collateral: Amount::from_sat(2_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create initial committee with two validators
    let initial_validators = vec![validator1, validator2];

    let mut committee = initial_validators.to_committee(&subnet_id, 0);

    // Verify initial state
    assert_eq!(committee.validators.len(), 2);
    assert_eq!(committee.total_power(), 25); // 10 + 15
    let initial_threshold = committee.threshold;
    let initial_address = committee.multisig_address.clone();

    // Create new validator to add
    let (secret_key3, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey3 = XOnlyPublicKey::from_keypair(&secret_key3.keypair(&secp)).0;

    let validator3 = SubnetValidator {
        pubkey: pubkey3,
        subnet_address: create_rand_eth_addr(),
        power: 20,
        collateral: Amount::from_sat(3_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8002".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Add the new validator
    committee
        .join_new_validator(&subnet_id, &validator3)
        .unwrap();

    // Verify the validator was added
    assert_eq!(committee.validators.len(), 3);
    assert!(committee.is_validator(&pubkey3));

    // Verify the power and threshold updated
    assert_eq!(committee.total_power(), 45); // 10 + 15 + 20
    assert!(committee.threshold > initial_threshold);

    // Verify multisig address changed
    assert_ne!(committee.multisig_address, initial_address);

    // Try adding an existing validator (should fail)
    let result = committee.join_new_validator(&subnet_id, &validator3);
    assert!(result.is_err());

    // Verify committee size didn't change after failed addition
    assert_eq!(committee.validators.len(), 3);
}

#[test]
fn test_subnet_state_rotation() {
    // Setup: Create a subnet ID
    let subnet_id = generate_subnet_id();

    // Create validators for the current committee
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
    let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
    let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

    let validator1 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 10,
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    let validator2 = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 15,
        collateral: Amount::from_sat(2_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create validators for the next committee (including a new one)
    let (secret_key3, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey3 = XOnlyPublicKey::from_keypair(&secret_key3.keypair(&secp)).0;

    let validator3 = SubnetValidator {
        pubkey: pubkey3,
        subnet_address: create_rand_eth_addr(),
        power: 20,
        collateral: Amount::from_sat(3_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8002".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create current committee with validators 1 and 2
    let current_validators = vec![validator1.clone(), validator2.clone()];
    let current_committee = current_validators.to_committee(&subnet_id, 1);

    // Create next committee with validators 1 and 3
    let next_validators = vec![validator1.clone(), validator3.clone()];
    let next_committee = next_validators.to_committee(&subnet_id, 2);

    // Create subnet state
    let mut subnet_state = SubnetState {
        id: subnet_id,
        committee_number: 1,
        committee: current_committee.clone(),
        waiting_committee: Some(next_committee.clone()),
        last_checkpoint_number: None,
        killed: SubnetKillState::NotKilled,
        marked_for_kill_checkpoint_number: None,
        epoch: 1,
        h_0: Some(0),
        last_rotation_epoch: 1,
        pending_multisig_changes: Vec::new(),
    };

    // Verify needs_rotation returns true when committees differ
    assert!(subnet_state.needs_rotation());

    // Verify original state
    assert_eq!(subnet_state.committee_number, 1);
    assert!(subnet_state.committee.is_validator(&pubkey1));
    assert!(subnet_state.committee.is_validator(&pubkey2));
    assert!(!subnet_state.committee.is_validator(&pubkey3));

    // The rotation checkpoint of epoch 2 adopts the waiting committee as committee 2
    let next = subnet_state.waiting_committee.take().unwrap();
    subnet_state.adopt_committee(2, 2, next);

    // Verify committee was updated
    assert_eq!(subnet_state.committee_number, 2);
    assert_eq!(subnet_state.last_rotation_epoch, 2);
    assert!(subnet_state.committee.is_validator(&pubkey1));
    assert!(!subnet_state.committee.is_validator(&pubkey2)); // No longer in committee
    assert!(subnet_state.committee.is_validator(&pubkey3)); // New validator added

    // Verify next_committee was consumed
    assert!(subnet_state.waiting_committee.is_none());

    // Verify needs_rotation now returns false
    assert!(!subnet_state.needs_rotation());
}

#[test]
fn test_should_kill_subnet() {
    let mut subnet = generate_subnet(3);
    assert!(!subnet.should_kill());
    subnet.killed = SubnetKillState::ToBeKilled;
    assert!(!subnet.should_kill());
    // With marked_for_kill=None, should_kill when (last_checkpoint+1) >= KILL_PERIOD
    subnet.last_checkpoint_number = Some(KILL_PERIOD_NUMBER_OF_CHECKPOINTS - 2);
    assert!(!subnet.should_kill());
    subnet.last_checkpoint_number = Some(KILL_PERIOD_NUMBER_OF_CHECKPOINTS);
    assert!(subnet.should_kill());
    let marked = 4u64;
    subnet.marked_for_kill_checkpoint_number = Some(marked);
    subnet.last_checkpoint_number = Some(marked); // last - marked = 0 < KILL_PERIOD
    assert!(!subnet.should_kill());
    subnet.marked_for_kill_checkpoint_number = Some(4);
    subnet.last_checkpoint_number = Some(10);
    assert!(subnet.should_kill());
    subnet.marked_for_kill_checkpoint_number = Some(4);
    subnet.last_checkpoint_number = Some(4 + KILL_PERIOD_NUMBER_OF_CHECKPOINTS - 1);
    assert!(!subnet.should_kill());
    subnet.marked_for_kill_checkpoint_number = Some(4);
    subnet.last_checkpoint_number = Some(4 + KILL_PERIOD_NUMBER_OF_CHECKPOINTS);
    assert!(subnet.should_kill());
    subnet.marked_for_kill_checkpoint_number = None;
    subnet.last_checkpoint_number = Some(KILL_PERIOD_NUMBER_OF_CHECKPOINTS - 1);
    assert!(subnet.should_kill());
}

#[test]
fn test_different_power_different_multisig() {
    // Setup: Create a subnet ID
    let subnet_id = generate_subnet_id();

    // Create validators
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
    let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
    let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

    // Create first set of validators with certain powers
    let validator1_set1 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 10, // Power of 10 for first set
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    let validator2_set1 = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 15, // Power of 15 for first set
        collateral: Amount::from_sat(1_500_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create second set of validators with the same pubkeys but different powers
    let validator1_set2 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 20, // Power of 20 for second set
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    let validator2_set2 = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 25, // Power of 25 for second set
        collateral: Amount::from_sat(2_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create the two committees
    let validators_set1 = vec![validator1_set1, validator2_set1];
    let validators_set2 = vec![validator1_set2, validator2_set2];

    let committee1 = validators_set1.to_committee(&subnet_id, 0);
    let committee2 = validators_set2.to_committee(&subnet_id, 0);

    // Verify both committees have the same validators (by pubkey)
    assert_eq!(committee1.pubkeys(), committee2.pubkeys());

    // Verify the committees have different total powers
    assert_eq!(committee1.total_power(), 25); // 10 + 15
    assert_eq!(committee2.total_power(), 45); // 20 + 25

    // Verify the committees have different thresholds
    assert_ne!(committee1.threshold, committee2.threshold);

    // Verify the multisig addresses are different
    assert_ne!(committee1.multisig_address, committee2.multisig_address);
}

#[test]
fn test_committee_modify_validator() {
    // Setup: Create a subnet ID and initial committee
    let subnet_id = generate_subnet_id();

    // Create secp context for key generation
    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Create two validators
    let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
    let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
    let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

    // Create the first validator
    let validator1 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 10,
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: Txid::all_zeros(),
    };

    // Create the second validator
    let validator2 = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 15,
        collateral: Amount::from_sat(2_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: Txid::all_zeros(),
    };

    // Create committee with two validators
    let validators = vec![validator1.clone(), validator2.clone()];
    let mut committee = validators.to_committee(&subnet_id, 1);

    // Record the initial state
    let initial_config_number = committee.configuration_number;
    let initial_threshold = committee.threshold;
    let initial_multisig = committee.multisig_address.clone();

    // Create a modified version of validator1 with increased collateral and power
    let mut modified_validator = validator1.clone();
    modified_validator.collateral = Amount::from_sat(3_000_000);
    modified_validator.power = 20;

    // Modify the validator
    let result = committee.modify_validator(&subnet_id, &modified_validator);
    assert!(result.is_ok());

    // Verify the committee state after modification
    assert_eq!(committee.configuration_number, initial_config_number + 1);
    assert_ne!(committee.threshold, initial_threshold);
    assert_ne!(committee.multisig_address, initial_multisig);

    // Check that the validator was updated correctly
    let updated_validator = committee
        .validators
        .iter()
        .find(|v| v.pubkey == pubkey1)
        .unwrap();
    assert_eq!(updated_validator.collateral, Amount::from_sat(3_000_000));
    assert_eq!(updated_validator.power, 20);

    // The second validator should remain unchanged
    let unchanged_validator = committee
        .validators
        .iter()
        .find(|v| v.pubkey == pubkey2)
        .unwrap();
    assert_eq!(unchanged_validator.collateral, Amount::from_sat(2_000_000));
    assert_eq!(unchanged_validator.power, 15);
}

#[test]
fn test_committee_join_new_validator() {
    // Setup: Create a subnet ID and initial committee
    let subnet_id = generate_subnet_id();

    // Create secp context for key generation
    let secp = bitcoin::secp256k1::Secp256k1::new();

    // Create an initial validator
    let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;

    let validator1 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 10,
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create committee with one validator
    let mut committee = vec![validator1].to_committee(&subnet_id, 1);

    // Record the initial state
    let initial_config_number = committee.configuration_number;
    let initial_validator_count = committee.validators.len();
    let initial_power = committee.total_power();

    // Create a new validator to join
    let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

    let new_validator = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 15,
        collateral: Amount::from_sat(2_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Join the new validator
    let result = committee.join_new_validator(&subnet_id, &new_validator);
    assert!(result.is_ok());

    // Verify the committee state after joining
    assert_eq!(committee.configuration_number, initial_config_number + 2); // Increases by 2
    assert_eq!(committee.validators.len(), initial_validator_count + 1);
    assert_eq!(committee.total_power(), initial_power + new_validator.power);

    // Verify the new validator is in the committee
    let joined_validator = committee.validators.iter().find(|v| v.pubkey == pubkey2);
    assert!(joined_validator.is_some());
    assert_eq!(joined_validator.unwrap().power, 15);
}

#[test]
#[test_retry::retry(3)]
fn test_stake_change_configuration_number() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();

    // Create validators
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let (secret_key1, _) = secp.generate_keypair(&mut rand::thread_rng());
    let (secret_key2, _) = secp.generate_keypair(&mut rand::thread_rng());
    let pubkey1 = XOnlyPublicKey::from_keypair(&secret_key1.keypair(&secp)).0;
    let pubkey2 = XOnlyPublicKey::from_keypair(&secret_key2.keypair(&secp)).0;

    let validator1 = SubnetValidator {
        pubkey: pubkey1,
        subnet_address: create_rand_eth_addr(),
        power: 10, // Power of 10 for first set
        collateral: Amount::from_sat(1_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8000".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    let validator2 = SubnetValidator {
        pubkey: pubkey2,
        subnet_address: create_rand_eth_addr(),
        power: 15,
        collateral: Amount::from_sat(2_000_000),
        backup_address: Address::from_str("bcrt1qpufku8sca56kmxylyd2233mfmnr9eyc4wdsdmd")
            .unwrap(),
        ip: "127.0.0.1:8001".parse().unwrap(),
        join_txid: create_rand_txid(),
    };

    // Create initial committee with two validators
    let initial_validators = vec![validator1.clone()];

    let mut committee = initial_validators.to_committee(&subnet_id, 0);

    // Initial check - should be GENESIS_COMMITTEE_CONF_NUM + 1
    let next_conf_num = db
        .get_next_stake_change_configuration_number(subnet_id)
        .unwrap();
    assert_eq!(next_conf_num, GENESIS_COMMITTEE_CONF_NUM + 1);

    // Join new validator (should increase configuration by 2)
    committee
        .join_new_validator(&subnet_id, &validator2)
        .unwrap();

    // Create a stake change request for joining
    let join_request = StakeChangeRequest {
        change: StakingChange::Join {
            pubkey: validator2.pubkey.public_key(Parity::Even),
        },
        validator_xpk: validator2.pubkey,
        validator_subnet_address: validator2.subnet_address,
        configuration_number: GENESIS_COMMITTEE_CONF_NUM + 1, // First change is metadata
        committee_after_change: committee.clone(),
        block_height: 100,
        block_hash: BlockHash::all_zeros(),
        checkpoint_block_height: None,
        checkpoint_block_hash: None,
        txid: Txid::all_zeros(),
    };

    // Create a stake change request for initial deposit
    let deposit_request = StakeChangeRequest {
        change: StakingChange::Deposit {
            amount: validator2.collateral,
        },
        validator_xpk: validator2.pubkey,
        validator_subnet_address: validator2.subnet_address,
        configuration_number: GENESIS_COMMITTEE_CONF_NUM + 2, // Second change is deposit
        committee_after_change: committee.clone(),
        block_height: 100,
        block_hash: BlockHash::all_zeros(),
        checkpoint_block_height: None,
        checkpoint_block_hash: None,
        txid: Txid::all_zeros(),
    };

    {
        let mut wtxn = db.write_txn().unwrap();
        // Store the join request
        db.add_stake_change(&mut wtxn, subnet_id, join_request)
            .unwrap();
        // Store the deposit request
        db.add_stake_change(&mut wtxn, subnet_id, deposit_request)
            .unwrap();
        wtxn.commit().unwrap();
    }

    // Check the configuration number after adding validator
    let next_conf_num = db
        .get_next_stake_change_configuration_number(subnet_id)
        .unwrap();
    assert_eq!(next_conf_num, GENESIS_COMMITTEE_CONF_NUM + 3);

    // Modify validator by increasing stake
    let mut updated_validator = validator1.clone();
    updated_validator.power = 20;
    updated_validator.collateral = Amount::from_sat(2_000_000);

    // Modify the validator in the committee
    committee
        .modify_validator(&subnet_id, &updated_validator)
        .unwrap();

    // Create a stake change request for the modification
    let modify_request = StakeChangeRequest {
        change: StakingChange::Deposit {
            amount: Amount::from_sat(1_000_000), // Additional 0.01 BTC
        },
        validator_xpk: validator1.pubkey,
        validator_subnet_address: validator1.subnet_address,
        configuration_number: GENESIS_COMMITTEE_CONF_NUM + 3, // Third change
        committee_after_change: committee.clone(),
        block_height: 101,
        block_hash: BlockHash::all_zeros(),
        checkpoint_block_height: None,
        checkpoint_block_hash: None,
        txid: Txid::all_zeros(),
    };

    {
        let mut wtxn = db.write_txn().unwrap();
        // Store the join request
        db.add_stake_change(&mut wtxn, subnet_id, modify_request)
            .unwrap();
        wtxn.commit().unwrap();
    }

    // Check the configuration number after modifying validator
    let next_conf_num = db
        .get_next_stake_change_configuration_number(subnet_id)
        .unwrap();
    assert_eq!(next_conf_num, GENESIS_COMMITTEE_CONF_NUM + 4);
}

#[test]
#[test_retry::retry(3)]
fn test_unified_nonce_across_message_types() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    let block_height = 100;
    let block_hash = create_rand_blockhash();
    let txid = create_rand_txid();

    let home_subnet_id = generate_subnet_id();

    // Add a FundSubnet message (nonce 0)
    let fund_msg = RootnetMessage::FundSubnet {
        msg: IpcFundSubnetMsg {
            subnet_id,
            amount: Amount::from_sat(1_000_000),
            address: create_rand_eth_addr(),
        },
        block_height,
        block_hash,
        nonce: 0,
        txid,
    };
    let mut txn = db.write_txn().unwrap();
    db.add_rootnet_msg(&mut txn, subnet_id, fund_msg).unwrap();

    // Add an ErcRegistration message (nonce 1)
    let erc_reg = RootnetMessage::ErcRegistration {
        home_subnet_id,
        registration: create_rand_erc_token_registration(),
        block_height,
        block_hash,
        nonce: 1,
        txid,
    };
    db.add_rootnet_msg(&mut txn, subnet_id, erc_reg).unwrap();

    // Add an ErcTransfer message (nonce 2)
    let erc_transfer = RootnetMessage::ErcTransfer {
        msg: create_rand_erc_transfer(home_subnet_id, subnet_id),
        block_height,
        block_hash,
        nonce: 2,
        txid,
    };
    db.add_rootnet_msg(&mut txn, subnet_id, erc_transfer)
        .unwrap();
    txn.commit().unwrap();

    // Verify next nonce is 3
    assert_eq!(db.get_next_rootnet_msg_nonce(subnet_id).unwrap(), 3);

    // Verify all messages are retrievable and nonces are correct
    let all_msgs = db.get_all_rootnet_msgs(subnet_id).unwrap();
    assert_eq!(all_msgs.len(), 3);
    assert_eq!(all_msgs[0].nonce(), 0);
    assert_eq!(all_msgs[1].nonce(), 1);
    assert_eq!(all_msgs[2].nonce(), 2);

    // Verify types
    assert!(matches!(all_msgs[0], RootnetMessage::FundSubnet { .. }));
    assert!(matches!(
        all_msgs[1],
        RootnetMessage::ErcRegistration { .. }
    ));
    assert!(matches!(all_msgs[2], RootnetMessage::ErcTransfer { .. }));
}

#[test]
#[test_retry::retry(3)]
fn test_get_rootnet_msgs_by_height_filters_all_variants() {
    let db = create_test_db();
    let subnet_id = generate_subnet_id();
    let home_subnet_id = generate_subnet_id();
    let block_hash = create_rand_blockhash();
    let txid = create_rand_txid();

    let height_100 = 100u64;
    let height_200 = 200u64;

    let mut txn = db.write_txn().unwrap();

    // Block 100: FundSubnet + ErcRegistration
    db.add_rootnet_msg(
        &mut txn,
        subnet_id,
        RootnetMessage::FundSubnet {
            msg: IpcFundSubnetMsg {
                subnet_id,
                amount: Amount::from_sat(500_000),
                address: create_rand_eth_addr(),
            },
            block_height: height_100,
            block_hash,
            nonce: 0,
            txid,
        },
    )
    .unwrap();

    db.add_rootnet_msg(
        &mut txn,
        subnet_id,
        RootnetMessage::ErcRegistration {
            home_subnet_id,
            registration: create_rand_erc_token_registration(),
            block_height: height_100,
            block_hash,
            nonce: 1,
            txid,
        },
    )
    .unwrap();

    // Block 200: ErcTransfer only
    db.add_rootnet_msg(
        &mut txn,
        subnet_id,
        RootnetMessage::ErcTransfer {
            msg: create_rand_erc_transfer(home_subnet_id, subnet_id),
            block_height: height_200,
            block_hash,
            nonce: 2,
            txid,
        },
    )
    .unwrap();

    txn.commit().unwrap();

    // Block 100 should have 2 messages (FundSubnet + ErcRegistration)
    let msgs_100 = db
        .get_rootnet_msgs_by_height(subnet_id, height_100)
        .unwrap();
    assert_eq!(msgs_100.len(), 2);
    assert!(matches!(msgs_100[0], RootnetMessage::FundSubnet { .. }));
    assert!(matches!(
        msgs_100[1],
        RootnetMessage::ErcRegistration { .. }
    ));

    // Block 200 should have 1 message (ErcTransfer)
    let msgs_200 = db
        .get_rootnet_msgs_by_height(subnet_id, height_200)
        .unwrap();
    assert_eq!(msgs_200.len(), 1);
    assert!(matches!(msgs_200[0], RootnetMessage::ErcTransfer { .. }));

    // Block 300 should have 0 messages
    let msgs_300 = db.get_rootnet_msgs_by_height(subnet_id, 300).unwrap();
    assert_eq!(msgs_300.len(), 0);
}

fn assert_unregistered(db: &HeedDb, id: SubnetId, owner: u8, eots: u8, accepted_count: usize) {
    assert!(db
        .get_ue_collaterals(id)
        .unwrap()
        .iter()
        .all(|c| c.pubkey != pubkey(owner)));
    assert!(db
        .get_eots_commitments(id, &pubkey(eots))
        .unwrap()
        .is_empty());
    assert_eq!(
        db.get_subnet_genesis_info(id)
            .unwrap()
            .unwrap()
            .genesis_validators
            .len(),
        accepted_count
    );
    assert!(db.get_subnet_state(id).unwrap().is_none());
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

#[tokio::test]
async fn accepted_join_records_the_exact_eots_commitment_and_its_provenance() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let join = checked(id, 1, 20, 11);
    process(&db, &join, 21).unwrap();
    let records = db.get_eots_commitments(id, &pubkey(20)).unwrap();
    assert_eq!(
        records,
        vec![EotsCommitmentRecord {
            subnet_id: id,
            pk_eots: pubkey(20),
            commitment: join.msg.nonce_commitment,
            block_height: 102,
            txid: Txid::from_byte_array([21; 32]),
        }]
    );
    assert!(db.get_eots_commitments(id, &pubkey(1)).unwrap().is_empty());
    assert!(db.get_subnet_state(id).unwrap().is_none());
}

#[tokio::test]
async fn eots_key_reuse_by_another_validator_rejects_all_join_state() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    process(&db, &checked(id, 1, 20, 11), 21).unwrap();
    let before = db.get_eots_commitments(id, &pubkey(20)).unwrap();
    assert!(process(&db, &checked(id, 2, 20, 12), 22).is_err());
    assert_eq!(db.get_ue_collaterals(id).unwrap().len(), 1);
    assert_eq!(db.get_eots_commitments(id, &pubkey(20)).unwrap(), before);
    assert_eq!(
        db.get_subnet_genesis_info(id)
            .unwrap()
            .unwrap()
            .genesis_validators
            .len(),
        1
    );
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

#[tokio::test]
async fn eots_key_used_in_another_subnet_cannot_register_a_different_validator() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    create(&db, a);
    create(&db, b);
    process(&db, &checked(a, 1, 20, 11), 21).unwrap();
    assert!(process(&db, &checked(b, 2, 20, 12), 22).is_err());
    assert_unregistered(&db, b, 2, 20, 0);
    assert_eq!(db.get_ue_collaterals(a).unwrap().len(), 1);
}

#[tokio::test]
async fn eots_key_used_in_another_subnet_cannot_register_its_original_validator() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    create(&db, a);
    create(&db, b);
    process(&db, &checked(a, 1, 20, 11), 21).unwrap();
    assert!(process(&db, &checked(b, 1, 20, 12), 22).is_err());
    assert_unregistered(&db, b, 1, 20, 0);
}

#[tokio::test]
async fn fresh_eots_key_allows_the_same_validator_to_join_a_different_subnet() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    create(&db, a);
    create(&db, b);
    process(&db, &checked(a, 1, 20, 11), 21).unwrap();
    process(&db, &checked(b, 1, 21, 12), 22).unwrap();
    assert_eq!(db.get_ue_collaterals(a).unwrap().len(), 1);
    assert_eq!(db.get_ue_collaterals(b).unwrap().len(), 1);
    assert_eq!(db.get_eots_commitments(b, &pubkey(21)).unwrap().len(), 1);
}

#[tokio::test]
async fn rejected_low_collateral_join_does_not_consume_the_eots_identity() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let mut low = checked(id, 1, 20, 11);
    low.msg.join.collateral = Amount::from_sat(99_999);
    assert!(process(&db, &low, 21).is_err());
    assert_unregistered(&db, id, 1, 20, 0);
    process(&db, &checked(id, 2, 20, 12), 22).unwrap();
    assert_eq!(db.get_ue_collaterals(id).unwrap()[0].pubkey, pubkey(2));
}

#[tokio::test]
async fn rejected_non_whitelisted_join_does_not_consume_the_eots_identity() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    assert!(process(&db, &checked(id, 7, 20, 11), 21).is_err());
    assert_unregistered(&db, id, 7, 20, 0);
    process(&db, &checked(id, 1, 20, 12), 22).unwrap();
    assert_eq!(db.get_eots_commitments(id, &pubkey(20)).unwrap().len(), 1);
}

#[tokio::test]
async fn accepted_eots_commitment_retains_its_provenance_after_reopening() {
    let (dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let join = checked(id, 1, 20, 11);
    process(&db, &join, 21).unwrap();
    drop(db);
    let db = HeedDb::new(dir.path().to_str().unwrap(), false)
        .await
        .unwrap();
    assert_eq!(
        db.get_eots_commitments(id, &pubkey(20)).unwrap(),
        vec![EotsCommitmentRecord {
            subnet_id: id,
            pk_eots: pubkey(20),
            commitment: join.msg.nonce_commitment,
            block_height: 102,
            txid: Txid::from_byte_array([21; 32]),
        }]
    );
}

#[tokio::test]
async fn commitment_query_does_not_drop_gapped_ranges_or_mix_keys_and_subnets() {
    let (_dir, db) = database().await;
    let (a, b) = (subnet(1), subnet(2));
    let mut txn = db.write_txn().unwrap();
    for (id, eots, start) in [
        (a, 20, 1000),
        (a, 20, 1),
        (a, 20, 30),
        (a, 21, 1),
        (b, 20, 1),
    ] {
        db.add_eots_commitment(
            &mut txn,
            &EotsCommitmentRecord {
                subnet_id: id,
                pk_eots: pubkey(eots),
                commitment: EotsNonceCommitment {
                    start,
                    count: 2,
                    root: [eots; 32],
                },
                block_height: 102,
                txid: Txid::from_byte_array([eots; 32]),
            },
        )
        .unwrap();
    }
    txn.commit().unwrap();
    let mut starts: Vec<_> = db
        .get_eots_commitments(a, &pubkey(20))
        .unwrap()
        .iter()
        .map(|r| r.commitment.start)
        .collect();
    starts.sort_unstable();
    assert_eq!(starts, vec![1, 30, 1000]);
    assert_eq!(db.get_eots_commitments(a, &pubkey(21)).unwrap().len(), 1);
    assert_eq!(db.get_eots_commitments(b, &pubkey(20)).unwrap().len(), 1);
    assert!(db.get_eots_commitments(b, &pubkey(21)).unwrap().is_empty());
}

#[tokio::test]
async fn deleted_eots_commitment_is_no_longer_returned_and_other_keys_remain() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    let mut txn = db.write_txn().unwrap();
    for eots in [20, 21] {
        db.add_eots_commitment(
            &mut txn,
            &EotsCommitmentRecord {
                subnet_id: id,
                pk_eots: pubkey(eots),
                commitment: EotsNonceCommitment {
                    start: 1,
                    count: 2,
                    root: [eots; 32],
                },
                block_height: 102,
                txid: Txid::from_byte_array([eots; 32]),
            },
        )
        .unwrap();
    }
    txn.commit().unwrap();
    let mut txn = db.write_txn().unwrap();
    db.delete_eots_commitment(&mut txn, id, &pubkey(20), 1)
        .unwrap();
    txn.commit().unwrap();
    assert!(db.get_eots_commitments(id, &pubkey(20)).unwrap().is_empty());
    assert_eq!(db.get_eots_commitments(id, &pubkey(21)).unwrap().len(), 1);
}

#[tokio::test]
async fn whitelist_configuration_threshold_does_not_grow_with_unused_whitelist_keys() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let config = db
        .get_subnet_genesis_info(id)
        .unwrap()
        .unwrap()
        .whitelist_configuration();
    assert_eq!(config.keys.len(), 6);
    assert!(config.keys.iter().all(|(_, power)| *power == 1));
    assert_eq!(config.threshold, 3); // floor(2 * 4 / 3) + 1
}


fn active_configuration(db: &HeedDb, id: SubnetId) {
    create(db, id);
    let mut genesis = db.get_subnet_genesis_info(id).unwrap().unwrap();
    genesis.bootstrapped = true;
    genesis.genesis_block_height = Some(101);
    genesis.genesis_validators = (1..=4)
        .map(|n| {
            let join = checked(id, n, n + 20, n + 10).msg.join;
            SubnetValidator {
                pubkey: join.pubkey,
                subnet_address: crate::eth_utils::eth_addr_from_x_only_pubkey(join.pubkey),
                power: 1,
                collateral: join.collateral,
                backup_address: join.backup_address,
                ip: join.ip,
                join_txid: Txid::from_byte_array([n + 30; 32]),
            }
        })
        .collect();
    let committee = genesis.genesis_validators.to_committee(&id, 0);
    assert_eq!(committee.threshold, 3); // floor(2 * 4 / 3) + 1
    let state = SubnetState {
        id,
        committee_number: 1,
        committee: committee.clone(),
        waiting_committee: None,
        last_checkpoint_number: Some(2),
        killed: SubnetKillState::NotKilled,
        marked_for_kill_checkpoint_number: None,
        epoch: 1,
        h_0: Some(0),
        last_rotation_epoch: 1,
        pending_multisig_changes: Vec::new(),
    };
    let mut txn = db.write_txn().unwrap();
    db.save_subnet_genesis_info(&mut txn, id, &genesis).unwrap();
    db.save_committee(&mut txn, id, 1, &committee).unwrap();
    db.save_subnet_state(&mut txn, id, &state).unwrap();
    txn.commit().unwrap();
}

fn accept_active_join(db: &HeedDb, id: SubnetId) -> UeCollateral {
    let mut join = checked(id, 5, 25, 15);
    join.committee_number = 1;
    process_join_ue(
        db,
        &join,
        Txid::from_byte_array([35; 32]),
        200,
        BlockHash::from_byte_array([0x43; 32]),
    )
    .unwrap();
    db.get_ue_collateral(id, Txid::from_byte_array([35; 32]), &pubkey(5))
        .unwrap()
        .unwrap()
}

fn presign_batch(id: SubnetId, entries: Vec<SlashSignatureEntry>) -> IpcBatchTransferMsg {
    IpcBatchTransferMsg {
        subnet_id: id,
        checkpoint_txid: Txid::from_byte_array([70; 32]),
        checkpoint_vout: 1,
        transfers: vec![],
        token_registrations: vec![],
        token_supply_adjustments: vec![],
        token_transfers: vec![],
        slash_signatures: entries,
    }
}

fn genesis_presign_batch(db: &HeedDb, id: SubnetId, signers: &[u8]) -> IpcBatchTransferMsg {
    let join = checked(id, 1, 21, 11);
    process(db, &join, 31).unwrap();
    let record = db.get_ue_collaterals(id).unwrap().pop().unwrap();
    presign_batch(id, vec![slash_entry(db, &record, signers)])
}

#[tokio::test]
async fn two_whitelisted_signatures_do_not_make_a_genesis_collateral_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let batch = genesis_presign_batch(&db, id, &[1, 5]);
    assert!(batch.newly_slashable(&db).unwrap().is_empty());
}

#[tokio::test]
async fn three_of_six_whitelisted_keys_make_a_genesis_collateral_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    create(&db, id);
    let batch = genesis_presign_batch(&db, id, &[1, 5, 6]);
    let genesis = db.get_subnet_genesis_info(id).unwrap().unwrap();
    assert_eq!(genesis.genesis_validators.len(), 1);
    assert_eq!(genesis.genesis_validators[0].pubkey, pubkey(1));
    let newly = batch.newly_slashable(&db).unwrap();
    assert_eq!(newly.len(), 1);
    assert_eq!(newly[0].0.pubkey, pubkey(1));
    assert_eq!(newly[0].1.len(), 3);
}

#[tokio::test]
async fn accepted_active_join_without_l1_presignatures_changes_no_configuration() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    assert_eq!(
        record.outpoint,
        OutPoint::new(Txid::from_byte_array([15; 32]), 1)
    );
    assert!(db
        .get_slash_presigs(id, &record.outpoint)
        .unwrap()
        .is_none());
    let state = db.get_subnet_state(id).unwrap().unwrap();
    assert!(!state.committee.is_validator(&pubkey(5)));
    assert!(state.waiting_committee.is_none());
    assert_eq!(state.committee.configuration_number, 0);
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
    assert!(presign_batch(id, vec![])
        .newly_slashable(&db)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn an_accepted_but_unseated_validator_cannot_join_again() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    accept_active_join(&db, id);
    let mut second = checked(id, 5, 26, 16);
    second.committee_number = 1;
    assert!(process_join_ue(
        &db,
        &second,
        Txid::from_byte_array([36; 32]),
        201,
        BlockHash::from_byte_array([0x44; 32]),
    )
    .is_err());
    assert_eq!(db.get_ue_collaterals(id).unwrap().len(), 1);
    assert!(db.get_eots_commitments(id, &pubkey(26)).unwrap().is_empty());
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

#[tokio::test]
async fn two_existing_validators_do_not_make_the_joiner_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let batch = presign_batch(id, vec![slash_entry(&db, &record, &[1, 2])]);
    assert!(batch.newly_slashable(&db).unwrap().is_empty());
    assert!(db
        .get_slash_presigs(id, &record.outpoint)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn three_existing_validators_make_the_joiner_slashable_without_its_node() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let entry = slash_entry(&db, &record, &[1, 2, 3]);
    let batch = presign_batch(id, vec![entry.clone()]);
    let newly = batch.newly_slashable(&db).unwrap();
    assert_eq!(newly.len(), 1);
    assert_eq!(newly[0].0, record);
    assert_eq!(newly[0].1, entry.signatures);
}

#[tokio::test]
async fn a_bad_extra_presignature_rejects_a_quorum_entry() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let mut entry = slash_entry(&db, &record, &[1, 2, 3, 4]);
    entry.signatures[0].1 = [[0; 32]; 2];
    assert!(presign_batch(id, vec![entry]).newly_slashable(&db).is_err());
    assert!(db
        .get_slash_presigs(id, &record.outpoint)
        .unwrap()
        .is_none());
    assert!(db.get_all_stake_changes(id).unwrap().is_empty());
}

#[tokio::test]
async fn a_quorum_for_another_collateral_does_not_make_the_joiner_slashable() {
    let (_dir, db) = database().await;
    let id = subnet(1);
    active_configuration(&db, id);
    let record = accept_active_join(&db, id);
    let mut different = record.clone();
    different.outpoint.vout = 2;
    let mut entry = slash_entry(&db, &different, &[1, 2, 3]);
    entry.vout = 1;
    assert!(presign_batch(id, vec![entry]).newly_slashable(&db).is_err());
}

#[tokio::test]
async fn monitor_genesis_retains_create_parameters_before_bootstrap() {
    let (_dir, db) = database().await;
    let params = IpcCreateSubnetMsg {
        min_validator_stake: Amount::from_sat(10_000),
        power_unit_exponent: 4,
        min_validators: 2,
        bottomup_check_period: 13,
        active_validators_limit: 20,
        min_cross_msg_fee: Amount::from_sat(1),
        whitelist: (1..=4).map(pubkey).collect(),
        epoch_length: 17,
        min_timeout: 2,
        max_timeout: 129,
        min_deposit: Amount::from_sat(777),
        kill_vote_period: 128,
        kill_delay_period: 0,
    };
    params.validate().unwrap();
    let txid = Txid::from_byte_array([0x19; 32]);
    params.save_to_db(&db, 102, txid).unwrap();
    let id = SubnetId::from_txid(&txid);
    let g = db.get_subnet_genesis_info(id).unwrap().unwrap();
    let p = &g.create_subnet_msg;
    assert_eq!(p.power_unit_exponent, 4);
    assert_eq!(p.epoch_length, 17);
    assert_eq!((p.min_timeout, p.max_timeout), (2, 129));
    assert_eq!(p.min_deposit.to_sat(), 777);
    assert_eq!((p.kill_vote_period, p.kill_delay_period), (128, 0));
    assert_eq!(g.create_msg_block_height, 102);
    assert!(!g.bootstrapped);
    assert!(db.get_subnet_state(id).unwrap().is_none());
}
