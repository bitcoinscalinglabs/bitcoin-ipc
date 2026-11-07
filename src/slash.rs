//! The slash transaction of a collateral output and its pre-signatures.

use bitcoin::{
    absolute::LockTime,
    hashes::Hash,
    key::{Keypair, Secp256k1},
    secp256k1::{self, Message, SecretKey},
    sighash::{Prevouts, SighashCache, TapSighashType},
    transaction::{self, Transaction, TxIn, TxOut},
    Amount, OutPoint, ScriptBuf, Sequence, Txid, Witness,
};
use serde::{Deserialize, Serialize};

use crate::collateral::{CollateralError, CollateralScript};
use crate::eots::{signature_from_bytes, signature_to_bytes, Signature64};
use crate::ipc_lib::verify_schnorr_bytes;

/// The pay-to-anchor output script `51 02 4e 73`.
pub const ANCHOR_SCRIPT: [u8; 4] = [0x51, 0x02, 0x4e, 0x73];

/// The slash pre-signatures of one output, as a batch transfer carries them: the outpoint
/// (txid in internal byte order) and the signatures by the signer's index in the ascending
/// order of the named configuration's keys. In JSON, the txid is in its usual hex form and
/// the signatures are `{index, signature}` objects with the signature in hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSignatureEntry {
    pub txid: [u8; 32],
    pub vout: u32,
    pub signatures: Vec<(u16, Signature64)>,
}

#[derive(Serialize, Deserialize)]
struct SlashSignatureEntryWire {
    txid: [u8; 32],
    vout: u32,
    signatures: Vec<(u16, Signature64)>,
}

#[derive(Serialize, Deserialize)]
struct SlashSignatureJson {
    index: u16,
    signature: String,
}

#[derive(Serialize, Deserialize)]
struct SlashSignatureEntryJson {
    txid: Txid,
    vout: u32,
    signatures: Vec<SlashSignatureJson>,
}

impl Serialize for SlashSignatureEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if serializer.is_human_readable() {
            SlashSignatureEntryJson {
                txid: Txid::from_byte_array(self.txid),
                vout: self.vout,
                signatures: self
                    .signatures
                    .iter()
                    .map(|(index, signature)| SlashSignatureJson {
                        index: *index,
                        signature: hex::encode(signature_to_bytes(signature)),
                    })
                    .collect(),
            }
            .serialize(serializer)
        } else {
            SlashSignatureEntryWire {
                txid: self.txid,
                vout: self.vout,
                signatures: self.signatures.clone(),
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for SlashSignatureEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            let json = SlashSignatureEntryJson::deserialize(deserializer)?;
            let signatures = json
                .signatures
                .into_iter()
                .map(|s| {
                    let bytes: [u8; 64] = hex::decode(&s.signature)
                        .map_err(serde::de::Error::custom)?
                        .try_into()
                        .map_err(|_| serde::de::Error::custom("a signature has 64 bytes"))?;
                    Ok((s.index, signature_from_bytes(&bytes)))
                })
                .collect::<Result<Vec<_>, D::Error>>()?;
            Ok(Self {
                txid: json.txid.to_byte_array(),
                vout: json.vout,
                signatures,
            })
        } else {
            let wire = SlashSignatureEntryWire::deserialize(deserializer)?;
            Ok(Self {
                txid: wire.txid,
                vout: wire.vout,
                signatures: wire.signatures,
            })
        }
    }
}

impl SlashSignatureEntry {
    pub fn new(outpoint: OutPoint, signatures: Vec<(u16, Signature64)>) -> Self {
        Self {
            txid: outpoint.txid.to_byte_array(),
            vout: outpoint.vout,
            signatures,
        }
    }

    pub fn outpoint(&self) -> OutPoint {
        OutPoint::new(Txid::from_byte_array(self.txid), self.vout)
    }

    /// The entries are in ascending `(txid, vout)` order, each with its signatures in
    /// ascending index order, at most one per index.
    pub fn check_order(entries: &[Self]) -> Result<(), String> {
        for pair in entries.windows(2) {
            if (pair[0].txid, pair[0].vout) >= (pair[1].txid, pair[1].vout) {
                return Err("slash signature entries are not in ascending outpoint order".into());
            }
        }
        for entry in entries {
            for pair in entry.signatures.windows(2) {
                if pair[0].0 >= pair[1].0 {
                    return Err(format!(
                        "slash signatures of {} are not in ascending index order",
                        entry.outpoint()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The unsigned slash transaction of the output `outpoint` of value `value`.
pub fn slash_transaction(outpoint: OutPoint, value: Amount) -> Transaction {
    Transaction {
        version: transaction::Version(3),
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: outpoint,
            sequence: Sequence::MAX,
            ..Default::default()
        }],
        output: vec![
            TxOut {
                value,
                script_pubkey: ScriptBuf::from_bytes(vec![0x6a]),
            },
            TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::from_bytes(ANCHOR_SCRIPT.to_vec()),
            },
        ],
    }
}

/// The BIP341 script-path sighash of the slash transaction's input through the slash leaf.
pub fn slash_sighash(
    secp: &Secp256k1<secp256k1::All>,
    script: &CollateralScript,
    outpoint: OutPoint,
    value: Amount,
) -> Result<[u8; 32], CollateralError> {
    let tx = slash_transaction(outpoint, value);
    let prevout = TxOut {
        value,
        script_pubkey: script.script_pubkey(secp)?,
    };
    let sighash = SighashCache::new(&tx).taproot_script_spend_signature_hash(
        0,
        &Prevouts::All(&[prevout]),
        script.slash_leaf()?.tapscript_leaf_hash(),
        TapSighashType::Default,
    )?;
    Ok(sighash.to_byte_array())
}

/// Signs the slash transaction of the output with a member's key: the signer's index in the
/// configuration and its signature.
pub fn sign_slash(
    secp: &Secp256k1<secp256k1::All>,
    script: &CollateralScript,
    outpoint: OutPoint,
    value: Amount,
    keypair: &Keypair,
) -> Result<(u16, Signature64), CollateralError> {
    let (key, _) = keypair.x_only_public_key();
    let index = script.configuration.key_index(&key).ok_or_else(|| {
        CollateralError::InvalidSlash(format!(
            "{key} is not a member of the configuration the output names"
        ))
    })?;
    let digest = slash_sighash(secp, script, outpoint, value)?;
    let signature = secp.sign_schnorr(&Message::from_digest(digest), keypair);
    Ok((index, signature_from_bytes(&signature.serialize())))
}

/// Verifies a pre-signature by the member at `index` of the configuration the output names.
pub fn verify_slash_signature(
    secp: &Secp256k1<secp256k1::All>,
    script: &CollateralScript,
    outpoint: OutPoint,
    value: Amount,
    index: u16,
    signature: &Signature64,
) -> Result<(), CollateralError> {
    let keys = script.configuration.sorted_keys();
    let (key, _) = keys.get(usize::from(index)).ok_or_else(|| {
        CollateralError::InvalidSlash(format!(
            "index {index} is outside the configuration of {} keys",
            keys.len()
        ))
    })?;
    let digest = slash_sighash(secp, script, outpoint, value)?;
    verify_schnorr_bytes(&signature_to_bytes(signature), digest, key)
        .map_err(|e| CollateralError::InvalidSlash(format!("signature {index}: {e}")))
}

/// The total power of the signers of `signatures` in the configuration, for the threshold.
pub fn signed_power(script: &CollateralScript, signatures: &[(u16, Signature64)]) -> u64 {
    signatures
        .iter()
        .filter_map(|(index, _)| script.configuration.power_at(*index))
        .map(u64::from)
        .sum()
}

/// The slash transaction with its witness: the members' pre-signatures and the EOTS signature
/// under `eots_secret_key`. Anyone who holds the EOTS secret key completes it this way.
pub fn complete_slash_transaction(
    secp: &Secp256k1<secp256k1::All>,
    script: &CollateralScript,
    outpoint: OutPoint,
    value: Amount,
    signatures: &[(u16, Signature64)],
    eots_secret_key: &SecretKey,
) -> Result<Transaction, CollateralError> {
    let digest = slash_sighash(secp, script, outpoint, value)?;
    let eots_signature = secp.sign_schnorr(
        &Message::from_digest(digest),
        &eots_secret_key.keypair(secp),
    );
    let keys = script.configuration.sorted_keys();
    let mut witness = Witness::new();
    for index in (0..keys.len()).rev() {
        match signatures.iter().find(|(i, _)| usize::from(*i) == index) {
            Some((_, signature)) => witness.push(signature_to_bytes(signature)),
            None => witness.push([]),
        }
    }
    witness.push(eots_signature.serialize());
    let leaf = script.slash_leaf()?;
    witness.push(leaf.to_bytes());
    witness.push(script.control_block(secp, &leaf)?.serialize());
    let mut tx = slash_transaction(outpoint, value);
    tx.input[0].witness = witness;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multisig::Configuration;
    use bitcoin::XOnlyPublicKey;

    fn keypair(seed: u8) -> Keypair {
        let secp = Secp256k1::new();
        Keypair::from_secret_key(&secp, &SecretKey::from_slice(&[seed; 32]).unwrap())
    }

    fn xonly(seed: u8) -> XOnlyPublicKey {
        keypair(seed).x_only_public_key().0
    }

    fn script() -> CollateralScript {
        CollateralScript::new(
            xonly(1),
            xonly(2),
            1_000,
            Configuration {
                keys: vec![(xonly(3), 1), (xonly(4), 2), (xonly(5), 1)],
                threshold: 3,
            },
        )
    }

    fn verify_with_consensus(
        tx: &Transaction,
        prevout: &TxOut,
    ) -> Result<(), bitcoinconsensus::Error> {
        let utxos = [bitcoinconsensus::Utxo {
            script_pubkey: prevout.script_pubkey.as_bytes().as_ptr(),
            script_pubkey_len: prevout.script_pubkey.len() as u32,
            value: prevout.value.to_sat() as i64,
        }];
        bitcoinconsensus::verify_with_flags(
            prevout.script_pubkey.as_bytes(),
            prevout.value.to_sat(),
            &bitcoin::consensus::serialize(tx),
            Some(&utxos),
            0,
            bitcoinconsensus::VERIFY_ALL_PRE_TAPROOT | bitcoinconsensus::VERIFY_TAPROOT,
        )
    }

    #[test]
    fn slash_transaction_has_the_spec_format() {
        let outpoint = OutPoint::new(Txid::from_byte_array([9; 32]), 1);
        let tx = slash_transaction(outpoint, Amount::from_sat(5_000));
        assert_eq!(tx.version, transaction::Version(3));
        assert_eq!(tx.lock_time, LockTime::ZERO);
        assert_eq!(tx.input.len(), 1);
        assert_eq!(tx.input[0].previous_output, outpoint);
        assert_eq!(tx.input[0].sequence, Sequence::MAX);
        assert_eq!(tx.output[0].value, Amount::from_sat(5_000));
        assert_eq!(tx.output[0].script_pubkey.as_bytes(), &[0x6a]);
        assert_eq!(tx.output[1].value, Amount::ZERO);
        assert_eq!(tx.output[1].script_pubkey.as_bytes(), &ANCHOR_SCRIPT);
        let entry = SlashSignatureEntry::new(outpoint, vec![]);
        assert_eq!(entry.txid, [9; 32]);
        assert_eq!(entry.outpoint(), outpoint);
    }

    #[test]
    fn pre_signatures_complete_a_valid_slash_spend() {
        let secp = Secp256k1::new();
        let script = script();
        let outpoint = OutPoint::new(Txid::from_byte_array([9; 32]), 1);
        let value = Amount::from_sat(50_000);
        let prevout = TxOut {
            value,
            script_pubkey: script.script_pubkey(&secp).unwrap(),
        };
        // Members 4 (power 2) and 5 (power 1) reach the threshold 3
        let sig4 = sign_slash(&secp, &script, outpoint, value, &keypair(4)).unwrap();
        let sig5 = sign_slash(&secp, &script, outpoint, value, &keypair(5)).unwrap();
        for (index, signature) in [&sig4, &sig5] {
            verify_slash_signature(&secp, &script, outpoint, value, *index, signature).unwrap();
        }
        let sorted = script.configuration.sorted_keys();
        assert_eq!(sorted[usize::from(sig4.0)].0, xonly(4));
        assert_eq!(signed_power(&script, &[sig4, sig5]), 3);
        let eots_sk = SecretKey::from_slice(&[2; 32]).unwrap();
        let tx =
            complete_slash_transaction(&secp, &script, outpoint, value, &[sig4, sig5], &eots_sk)
                .unwrap();
        verify_with_consensus(&tx, &prevout).unwrap();
        assert_eq!(tx.input[0].witness.len(), 3 + 1 + 2);

        // Below the threshold, with the wrong EOTS key, or with a wrong index, the spend fails
        let only4 =
            complete_slash_transaction(&secp, &script, outpoint, value, &[sig4], &eots_sk).unwrap();
        assert!(verify_with_consensus(&only4, &prevout).is_err());
        let wrong_key = SecretKey::from_slice(&[1; 32]).unwrap();
        let wrong =
            complete_slash_transaction(&secp, &script, outpoint, value, &[sig4, sig5], &wrong_key)
                .unwrap();
        assert!(verify_with_consensus(&wrong, &prevout).is_err());
        assert!(verify_slash_signature(&secp, &script, outpoint, value, sig5.0, &sig4.1).is_err());
        assert!(verify_slash_signature(&secp, &script, outpoint, value, 7, &sig4.1).is_err());
        // Not a member
        assert!(sign_slash(&secp, &script, outpoint, value, &keypair(6)).is_err());
        // A signature for another value does not verify
        assert!(verify_slash_signature(
            &secp,
            &script,
            outpoint,
            Amount::from_sat(1),
            sig4.0,
            &sig4.1
        )
        .is_err());
    }

    #[test]
    fn entry_serializes_as_the_spec_tuple_and_as_json() {
        let entry = SlashSignatureEntry {
            txid: [1; 32],
            vout: 2,
            signatures: vec![(3, [[4; 32], [5; 32]])],
        };
        let mut expected = vec![1u8; 32];
        expected.push(2);
        expected.push(1);
        expected.push(3);
        expected.extend_from_slice(&[4; 32]);
        expected.extend_from_slice(&[5; 32]);
        let bytes = postcard::to_stdvec(&entry).unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(
            postcard::from_bytes::<SlashSignatureEntry>(&bytes).unwrap(),
            entry
        );
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(
            json["txid"],
            serde_json::Value::String(Txid::from_byte_array([1; 32]).to_string())
        );
        assert_eq!(json["signatures"][0]["index"], 3);
        assert_eq!(
            json["signatures"][0]["signature"].as_str().unwrap().len(),
            128
        );
        assert_eq!(
            serde_json::from_value::<SlashSignatureEntry>(json).unwrap(),
            entry
        );
    }

    #[test]
    fn entry_order_is_checked() {
        let a = SlashSignatureEntry {
            txid: [1; 32],
            vout: 0,
            signatures: vec![(0, [[0; 32]; 2]), (2, [[0; 32]; 2])],
        };
        let b = SlashSignatureEntry {
            txid: [1; 32],
            vout: 1,
            signatures: vec![],
        };
        SlashSignatureEntry::check_order(&[a.clone(), b.clone()]).unwrap();
        assert!(SlashSignatureEntry::check_order(&[b.clone(), a.clone()]).is_err());
        assert!(SlashSignatureEntry::check_order(&[a.clone(), a.clone()]).is_err());
        let dup = SlashSignatureEntry {
            txid: [1; 32],
            vout: 0,
            signatures: vec![(2, [[0; 32]; 2]), (2, [[0; 32]; 2])],
        };
        assert!(SlashSignatureEntry::check_order(&[dup]).is_err());
    }
}
#[cfg(test)]
#[path = "slash_spec_tests.rs"]
mod spec_tests;
