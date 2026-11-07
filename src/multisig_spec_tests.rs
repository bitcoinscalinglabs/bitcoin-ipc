use super::{multisig_threshold, Configuration};
use bitcoin::secp256k1::{Secp256k1, SecretKey, XOnlyPublicKey};

fn keys() -> Vec<XOnlyPublicKey> {
    let secp = Secp256k1::new();
    let mut keys: Vec<_> = (1..=4)
        .map(|n| {
            let mut bytes = [0; 32];
            bytes[31] = n;
            SecretKey::from_slice(&bytes)
                .unwrap()
                .x_only_public_key(&secp)
                .0
        })
        .collect();
    keys.sort_by_key(|k| k.serialize());
    keys
}

// batch-transfer.md, Transaction format (slash_signatures): indices are zero-based ascending key positions.
#[test]
fn signature_indices_follow_key_bytes_instead_of_insertion_order_or_weight() {
    let k = keys();
    let c = Configuration {
        keys: vec![(k[2], 99), (k[0], 7), (k[3], 1), (k[1], 40)],
        threshold: 99,
    };
    for (i, key) in k.iter().enumerate() {
        assert_eq!(c.key_index(key), Some(i as u16));
    }
    assert_eq!(c.power_at(0), Some(7));
    assert_eq!(c.power_at(1), Some(40));
    assert_eq!(c.power_at(2), Some(99));
    assert_eq!(c.power_at(3), Some(1));
}

// batch-transfer.md, Transaction format (slash_signatures): a key outside the named configuration has no index.
#[test]
fn absent_configuration_key_does_not_alias_an_existing_signature_index() {
    let k = keys();
    let c = Configuration {
        keys: vec![(k[2], 2), (k[0], 1)],
        threshold: 3,
    };
    assert_eq!(c.key_index(&k[1]), None);
    assert_eq!(c.key_index(&k[3]), None);
}

// batch-transfer.md, Transaction format (slash_signatures): an index must name an actual configuration member.
#[test]
fn signature_indices_at_the_length_and_u16_maximum_have_no_member() {
    let k = keys();
    let c = Configuration {
        keys: vec![(k[0], 1), (k[1], 2)],
        threshold: 3,
    };
    assert_eq!(c.power_at(2), None);
    assert_eq!(c.power_at(u16::MAX), None);
}

// lifecycle.md, Power and thresholds: floor(2P/3)+1 remains exact at MAX_TOTAL_POWER.
#[test]
fn threshold_is_exact_at_maximum_total_power() {
    assert_eq!(multisig_threshold(2_147_483_645), 1_431_655_764);
    assert_eq!(multisig_threshold(2_147_483_646), 1_431_655_765);
    assert_eq!(multisig_threshold(2_147_483_647), 1_431_655_765);
}
