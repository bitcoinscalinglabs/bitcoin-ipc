// The length of the ethereum address - helper
pub const ETH_ADDR_LEN: usize = alloy_primitives::Address::len_bytes();

// fvm_shared rellies on this global current_network variable
// to format the address correctly
//
// if the address is not formatted correctly
// ipc will not be able to parse it back
pub fn set_fvm_network() {
    use fvm_shared::address::{set_current_network, Network as FvmNetwork};
    let network = if crate::NETWORK == bitcoin::Network::Bitcoin {
        FvmNetwork::Mainnet
    } else {
        FvmNetwork::Testnet
    };
    log::debug!("Set fvm network to {network:?}");
    set_current_network(network);
}

/// Derives the Ethereum address from a Bitcoin x-only public key
// TODO from_raw_public_key could panic if pubkey.len() is not 64
pub fn eth_addr_from_x_only_pubkey(pubkey: bitcoin::XOnlyPublicKey) -> alloy_primitives::Address {
    // In Bitcoin, XOnlyPublicKey is assumed to have even parity
    let pubkey = pubkey.public_key(bitcoin::key::Parity::Even);
    // Remove the prefix
    let pubkey = &pubkey.serialize_uncompressed()[1..];

    alloy_primitives::Address::from_raw_public_key(pubkey)
}

pub fn delegated_fvm_to_eth_address(
    addr: &fvm_shared::address::Address,
) -> Option<alloy_primitives::Address> {
    match addr.payload() {
        fvm_shared::address::Payload::Delegated(d) if d.subaddress().len() == ETH_ADDR_LEN => {
            Some(alloy_primitives::Address::from_slice(d.subaddress()))
        }
        _ => None,
    }
}

pub fn evm_address_to_delegated_fvm(
    eth_addr: &alloy_primitives::Address,
    namespace: u64,
) -> fvm_shared::address::Address {
    // eth_addr is 20 bytes long so we can always convert it to fvm
    fvm_shared::address::Address::new_delegated(namespace, eth_addr.as_slice())
        .expect("eth address can always be converted to fvm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::Address;
    use bitcoin::key::{Keypair, XOnlyPublicKey};
    use bitcoin::secp256k1::Secp256k1;

    #[test]
    fn test_eth_addr_from_x_only_pubkey() {
        let secp = Secp256k1::new();

        // Keep generating keypairs until we get one with even parity
        let (keypair, x_only_pubkey) = loop {
            let keypair = Keypair::new(&secp, &mut rand::thread_rng());
            let (x_only_pubkey, parity) = XOnlyPublicKey::from_keypair(&keypair);

            if parity == bitcoin::key::Parity::Even {
                break (keypair, x_only_pubkey);
            }
        };
        let eth_addr = eth_addr_from_x_only_pubkey(x_only_pubkey);
        let pubkey = &keypair.public_key().serialize_uncompressed()[1..];
        let expected_addr = Address::from_raw_public_key(pubkey);
        assert_eq!(eth_addr, expected_addr);
    }

    #[test]
    fn test_eth_addr_from_x_only_pubkey_odd_parity() {
        let secp = Secp256k1::new();

        // Keep generating keypairs until we get one with odd parity
        let (keypair, x_only_pubkey) = loop {
            let keypair = Keypair::new(&secp, &mut rand::thread_rng());
            let (x_only_pubkey, parity) = XOnlyPublicKey::from_keypair(&keypair);

            if parity == bitcoin::key::Parity::Odd {
                break (keypair, x_only_pubkey);
            }
        };

        let eth_addr = eth_addr_from_x_only_pubkey(x_only_pubkey);
        let pubkey = &keypair.public_key().serialize_uncompressed()[1..];
        let expected_addr = Address::from_raw_public_key(pubkey);

        // The addresses should NOT match when parity is odd
        assert_ne!(eth_addr, expected_addr);
    }
}
