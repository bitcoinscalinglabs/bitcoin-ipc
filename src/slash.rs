//! The slash transaction of a collateral output and its pre-signatures.

use bitcoin::{
    absolute::LockTime,
    hashes::Hash,
    key::{Keypair, Secp256k1},
    secp256k1::{self, Message},
    sighash::{Prevouts, SighashCache, TapSighashType},
    transaction::{self, Transaction, TxIn, TxOut},
    Amount, OutPoint, ScriptBuf, Sequence, Txid,
};
#[cfg(test)]
use bitcoin::{secp256k1::SecretKey, Witness};
use serde::{Deserialize, Serialize};

use crate::bitcoin_utils::verify_schnorr_bytes;
use crate::collateral::{CollateralError, CollateralScript};
use crate::eots::{signature_from_bytes, signature_to_bytes, Signature64};

/// The pay-to-anchor output script `51 02 4e 73`.
pub const ANCHOR_SCRIPT: [u8; 4] = [0x51, 0x02, 0x4e, 0x73];

/// The slash pre-signatures of one collateral output as posted on the subnet: signatures only,
/// without their signers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostedSlashSignatures {
    pub txid: [u8; 32],
    pub vout: u32,
    pub signatures: Vec<Signature64>,
}

#[derive(Serialize, Deserialize)]
struct PostedSlashSignaturesJson {
    txid: Txid,
    vout: u32,
    signatures: Vec<String>,
}

impl Serialize for PostedSlashSignatures {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        PostedSlashSignaturesJson {
            txid: Txid::from_byte_array(self.txid),
            vout: self.vout,
            signatures: self
                .signatures
                .iter()
                .map(|signature| hex::encode(signature_to_bytes(signature)))
                .collect(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PostedSlashSignatures {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let json = PostedSlashSignaturesJson::deserialize(deserializer)?;
        let signatures = json
            .signatures
            .iter()
            .map(|signature| {
                let bytes: [u8; 64] = hex::decode(signature)
                    .map_err(serde::de::Error::custom)?
                    .try_into()
                    .map_err(|_| serde::de::Error::custom("a signature has 64 bytes"))?;
                Ok(signature_from_bytes(&bytes))
            })
            .collect::<Result<Vec<_>, D::Error>>()?;
        Ok(Self {
            txid: json.txid.to_byte_array(),
            vout: json.vout,
            signatures,
        })
    }
}

impl PostedSlashSignatures {
    pub fn new(outpoint: OutPoint, signatures: Vec<Signature64>) -> Self {
        Self {
            txid: outpoint.txid.to_byte_array(),
            vout: outpoint.vout,
            signatures,
        }
    }

    pub fn outpoint(&self) -> OutPoint {
        OutPoint::new(Txid::from_byte_array(self.txid), self.vout)
    }
}

/// The slash pre-signatures of one collateral output, each with its signer's index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashSignatureEntry {
    pub txid: [u8; 32],
    pub vout: u32,
    pub signatures: Vec<(u16, Signature64)>,
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
            (&self.txid, self.vout, &self.signatures).serialize(serializer)
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
            let (txid, vout, signatures) = Deserialize::deserialize(deserializer)?;
            Ok(Self {
                txid,
                vout,
                signatures,
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

    /// Checks that entries and their signature indexes are in strictly ascending order.
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

/// Splits the pre-signatures of the output into the valid ones and the indexes of the others,
/// with the total power of the valid signers in the configuration the output names.
pub fn check_slash_signatures(
    secp: &Secp256k1<secp256k1::All>,
    script: &CollateralScript,
    outpoint: OutPoint,
    value: Amount,
    signatures: &[(u16, Signature64)],
) -> Result<(Vec<(u16, Signature64)>, Vec<u16>, u64), CollateralError> {
    let keys = script.configuration.sorted_keys();
    let digest = slash_sighash(secp, script, outpoint, value)?;
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    let mut power = 0;
    for (index, signature) in signatures {
        match keys.get(usize::from(*index)) {
            Some((key, key_power))
                if verify_schnorr_bytes(&signature_to_bytes(signature), digest, key).is_ok() =>
            {
                valid.push((*index, *signature));
                power += u64::from(*key_power);
            }
            _ => invalid.push(*index),
        }
    }
    Ok((valid, invalid, power))
}

/// Resolves posted pre-signatures of the output to the keys of the configuration it names: each
/// signature with the index of the key for which it verifies, the first one per key, in ascending
/// order of index, with the total power of those keys and the number of signatures that verify
/// for no key without an earlier signature. A signature is checked only against the keys without
/// an earlier signature, and none once every key has one.
pub fn resolve_slash_signatures(
    secp: &Secp256k1<secp256k1::All>,
    script: &CollateralScript,
    outpoint: OutPoint,
    value: Amount,
    signatures: &[Signature64],
) -> Result<(Vec<(u16, Signature64)>, u64, usize), CollateralError> {
    let keys = script.configuration.sorted_keys();
    let msg = Message::from_digest(slash_sighash(secp, script, outpoint, value)?);
    let mut resolved: Vec<(u16, Signature64)> = Vec::new();
    let mut signed = vec![false; keys.len()];
    let mut power = 0;
    let mut unresolved = 0;
    for signature in signatures {
        if resolved.len() == keys.len() {
            break;
        }
        let sig = secp256k1::schnorr::Signature::from_slice(&signature_to_bytes(signature))
            .map_err(|e| CollateralError::InvalidSlash(e.to_string()))?;
        match crate::multisig::signer_of(secp, &keys, &msg, &sig, |i| signed[i]) {
            Some(i) => {
                signed[i] = true;
                resolved.push((i as u16, *signature));
                power += u64::from(keys[i].1);
            }
            None => unresolved += 1,
        }
    }
    resolved.sort_by_key(|(index, _)| *index);
    Ok((resolved, power, unresolved))
}

/// The slash transaction with its witness: the members' pre-signatures and the EOTS signature
/// under `eots_secret_key`.
#[cfg(test)]
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
        let (valid, invalid, power) =
            check_slash_signatures(&secp, &script, outpoint, value, &[sig4, sig5]).unwrap();
        assert_eq!((valid, invalid, power), (vec![sig4, sig5], vec![], 3));
        let sorted = script.configuration.sorted_keys();
        assert_eq!(sorted[usize::from(sig4.0)].0, xonly(4));
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
        let (_, invalid, _) = check_slash_signatures(
            &secp,
            &script,
            outpoint,
            value,
            &[(sig5.0, sig4.1), (7, sig4.1)],
        )
        .unwrap();
        assert_eq!(invalid, vec![sig5.0, 7]);
        // Not a member
        assert!(sign_slash(&secp, &script, outpoint, value, &keypair(6)).is_err());
        // A signature for another value does not verify
        let (_, invalid, _) =
            check_slash_signatures(&secp, &script, outpoint, Amount::from_sat(1), &[sig4]).unwrap();
        assert_eq!(invalid, vec![sig4.0]);
    }

    #[test]
    fn posted_signatures_resolve_to_their_keys() {
        let secp = Secp256k1::new();
        let script = script();
        let outpoint = OutPoint::new(Txid::from_byte_array([9; 32]), 1);
        let value = Amount::from_sat(50_000);
        let mut signed: Vec<_> = [3, 4, 5]
            .map(|seed| sign_slash(&secp, &script, outpoint, value, &keypair(seed)).unwrap())
            .to_vec();
        signed.sort_by_key(|(index, _)| *index);
        let bad = [[7; 32], [7; 32]];
        // In descending order of index, with a copy and a signature of no key; once every key has
        // a signature, the last one is not checked
        let posted = [signed[2].1, signed[1].1, bad, signed[2].1, signed[0].1, bad];
        let (resolved, power, unresolved) =
            resolve_slash_signatures(&secp, &script, outpoint, value, &posted).unwrap();
        assert_eq!((resolved, power, unresolved), (signed, 4, 2));
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

    fn wire_entry(vout: u32, index: u16, count: usize) -> SlashSignatureEntry {
        SlashSignatureEntry {
            txid: [0x12; 32],
            vout,
            signatures: (0..count)
                .map(|i| (index + i as u16, [[0x23; 32], [0x45; 32]]))
                .collect(),
        }
    }

    #[test]
    fn slash_entry_vout_uses_the_full_u32_varint_width() {
        for (vout, varint) in [
            (128, vec![0x80, 0x01]),
            (16384, vec![0x80, 0x80, 0x01]),
            (u32::MAX, vec![0xff, 0xff, 0xff, 0xff, 0x0f]),
        ] {
            let mut expected = vec![0x12; 32];
            expected.extend(varint);
            expected.extend([1, 0]); // one signature at index zero
            expected.extend([0x23; 32]);
            expected.extend([0x45; 32]);
            assert_eq!(
                postcard::to_stdvec(&wire_entry(vout, 0, 1)).unwrap(),
                expected
            );
            let parsed: SlashSignatureEntry = postcard::from_bytes(&expected).unwrap();
            assert_eq!(parsed.vout, vout);
        }
    }

    #[test]
    fn slash_entry_index_uses_the_full_u16_varint_width() {
        for (index, varint) in [
            (128, vec![0x80, 0x01]),
            (16384, vec![0x80, 0x80, 0x01]),
            (u16::MAX, vec![0xff, 0xff, 0x03]),
        ] {
            let mut expected = vec![0x12; 32];
            expected.extend([0, 1]); // vout zero, one signature
            expected.extend(varint);
            expected.extend([0x23; 32]);
            expected.extend([0x45; 32]);
            assert_eq!(
                postcard::to_stdvec(&wire_entry(0, index, 1)).unwrap(),
                expected
            );
            let parsed: SlashSignatureEntry = postcard::from_bytes(&expected).unwrap();
            assert_eq!(parsed.signatures[0].0, index);
        }
    }

    #[test]
    fn slash_entry_signature_count_crosses_the_single_byte_varint_boundary() {
        let mut expected = vec![0x12; 32];
        expected.extend([0, 0x80, 0x01]); // vout zero, 128 signatures
        for index in 0..128_u8 {
            expected.push(index);
            expected.extend([0x23; 32]);
            expected.extend([0x45; 32]);
        }
        assert_eq!(
            postcard::to_stdvec(&wire_entry(0, 0, 128)).unwrap(),
            expected
        );
    }
}
