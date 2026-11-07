use super::SlashSignatureEntry;

// These are wire-type tests only: the signature bytes are opaque, and no unimplemented
// slash leaf, slash spend, key extraction or collateral seating is exercised.
fn entry(vout: u32, index: u16, count: usize) -> SlashSignatureEntry {
    SlashSignatureEntry {
        txid: [0x12; 32],
        vout,
        signatures: (0..count)
            .map(|i| (index + i as u16, [[0x23; 32], [0x45; 32]]))
            .collect(),
    }
}

// batch-transfer.md, Transaction format: vout is unsigned LEB128 u32, including multi-byte boundaries.
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
        assert_eq!(postcard::to_stdvec(&entry(vout, 0, 1)).unwrap(), expected);
        let parsed: SlashSignatureEntry = postcard::from_bytes(&expected).unwrap();
        assert_eq!(parsed.vout, vout);
    }
}

// batch-transfer.md, Transaction format: the index wire type is u16, without truncation at 127 or 16383.
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
        assert_eq!(postcard::to_stdvec(&entry(0, index, 1)).unwrap(), expected);
        let parsed: SlashSignatureEntry = postcard::from_bytes(&expected).unwrap();
        assert_eq!(parsed.signatures[0].0, index);
    }
}

// encoding.md, Postcard encoding (list), and batch-transfer.md: 128 signatures use a two-byte count.
#[test]
fn slash_entry_signature_count_crosses_the_single_byte_varint_boundary() {
    let mut expected = vec![0x12; 32];
    expected.extend([0, 0x80, 0x01]); // vout zero, 128 signatures
    for index in 0..128_u8 {
        expected.push(index);
        expected.extend([0x23; 32]);
        expected.extend([0x45; 32]);
    }
    assert_eq!(postcard::to_stdvec(&entry(0, 0, 128)).unwrap(), expected);
}
