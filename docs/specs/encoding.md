# Encodings, scripts and signatures

Building blocks shared by the transaction documents in `transactions/`. [lifecycle.md](lifecycle.md) defines the lifecycle, the state and the configurations of a subnet.

## Notation

| Symbol | Meaning |
|---|---|
| `‖` | byte concatenation |
| `tagged_sha256(tag, m)` | `sha256(sha256(tag) ‖ sha256(tag) ‖ m)`, with `tag` an ASCII string |
| `r` | the fee rate of a transaction, in sat/vB (see [Fee rate](#fee-rate)) |
| `fee_r(w)` | `⌈250 · r · w / 1000⌉` sat, the fee of weight `w` at rate `r` |
| `dust_r(spk)` | `⌊(75 + len(ser(spk))) · r⌋` sat for a witness-program scriptPubKey `spk`, where `ser(spk)` is the script with its compact-size length prefix: `110 · r` for P2TR and P2WSH, `98 · r` for P2WPKH, and 0 for an OP_RETURN |
| `dust(spk)` | Bitcoin Core's default dust threshold of `spk` (rate 3 sat/vB): 330 sat for P2TR and P2WSH, 294 sat for P2WPKH, 0 for an OP_RETURN |
| `K_NUMS` | the unspendable internal key (see [Unspendable internal key](#unspendable-internal-key)) |
| `txid20(id)` | the 20 bytes inside a subnet id (see [Subnet id](#subnet-id)) |
| `MS(C)` | the multisig address of configuration `C` (see [Configuration multisig](#configuration-multisig)) |
| sat | satoshi, 10⁻⁸ BTC. On the subnet, 1 sat is 10¹⁰ of the subnet's smallest unit (1 BTC = 10¹⁸) |
| EOTS | extractable one-time signature: a BIP340 signature whose nonce is fixed in advance, so that two signatures with the same nonce reveal the secret key ([slashing](slashing.md)) |

Integers in OP_RETURN layouts are little-endian unless the layout says otherwise.

## Fee rate

Whoever builds a transaction chooses its fee rate `r`, in whole sat/vB, from its Bitcoin L1 node's fee estimate. A transaction paid by a wallet (create, prefund, join, stake, deposit) may use any fee its wallet chooses. `r` fixes the values that the protocol computes itself: commitment outputs, reveal outputs, configuration multisig spends and the exit.

Each provider uses its own estimate for a checkpoint, so two providers can build different transactions for the same checkpoint.

## Identifiers

### Subnet id

A subnet id is the string `L1 prefix ‖ "/" ‖ A`, where the L1 prefix names the network ([network parameters](lifecycle.md#network-parameters)) and `A` is an FVM delegated address with namespace 10 and a 20-byte subaddress `txid20`:

- `txid20` is the first 20 bytes, in internal byte order, of the txid of the create subnet reveal transaction. The usual hex form of a txid is byte-reversed, so `txid20` is the reverse of the last 40 hex characters of that form.
- The string form of `A` is `t410f` followed by the lowercase base32 encoding (RFC 4648, no padding) of `txid20` followed by its 4-byte FVM checksum. The checksum is blake2b with a 4-byte digest, not a truncation, over the protocol byte 4, the varint namespace 10 and `txid20`. Every subnet id of one network therefore has the same length. On regtest it is 48 characters, for example `/b4/t410fhor637l2pmjle6whfq7go5upmf74qg6dbr4uzei`.
- Binary fields that carry a subnet id hold only `txid20`.

### Validator subnet address

The subnet address of a validator key is its EVM address: the last 20 bytes of the Keccak-256 hash of the 64-byte uncompressed public key, taken with even Y. Validator keys are x-only keys with even Y.

## Postcard encoding

Message payloads are postcard (version 1) encodings of the structures given in each transaction document. A payload is valid only if it is exactly the encoding of its structure: the monitor decodes it and rejects it unless re-encoding the decoded value gives the same bytes. A payload with a missing field, a byte left over or a non-minimal varint is therefore invalid.

| Type | Encoding |
|---|---|
| `u8` | 1 byte |
| option | `0x00` when absent, `0x01` ‖ the value when present |
| `u16`, `u32`, `u64`, lengths | unsigned LEB128 varint (7 bits per byte, least significant group first) |
| `bool` | 1 byte, `0x00` or `0x01` |
| struct, tuple, fixed array | fields in order, no prefix |
| list | varint count ‖ elements |
| map | varint count ‖ (key ‖ value) per entry, entries in ascending key order |
| string | varint byte length ‖ UTF-8 bytes |
| enum | varint variant index ‖ content |

Types with their own encoding:

| Type | Encoding | Size (bytes) |
|---|---|---|
| subnet id | its string form | 1 + length (49 bytes on regtest) |
| x-only key | 32 bytes | 32 |
| Bitcoin address | its string form | 1 + length |
| EVM address | `0x14` ‖ 20 bytes | 21 |
| amount | `u64`, satoshis | 1–10 |
| socket address | `0x00` ‖ 4 bytes ‖ varint port (IPv4), `0x01` ‖ 16 bytes ‖ varint port (IPv6) | 6–8 or 18–20 |
| `txid20` | 20 bytes | 20 |
| compact U256 | varint `1 + N` ‖ `N` ‖ `m`, where `m` is the big-endian magnitude without leading zero bytes and `N = len(m) ≤ 32` | 2 + N |
| compact I256 | as compact U256, with the byte `N` replaced by `N | 0x80` for a negative value. Zero is `01 00`, a negative zero is invalid, and the value lies in `[−2²⁵⁵, 2²⁵⁵ − 1]` | 2 + N |

## Keys and scripts

### Unspendable internal key

`K_NUMS` is the x-only key `0x79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81799`, the x coordinate of the secp256k1 generator plus one: a valid x coordinate whose discrete logarithm nobody knows. Every taproot output of the protocol uses `K_NUMS` as internal key, so only a script path can spend it.

### OP_RETURN output

The OP_RETURN output of a message is `OP_RETURN` (`0x6a`) followed by one minimal push of its data (`OP_PUSHBYTES_n` for `n ≤ 75` bytes, `OP_PUSHDATA1 n` for `76 ≤ n ≤ 255`). Its value is 0, except output 0 of the kill checkpoint, which burns its value. A message in an OP_RETURN must be in output 0 and must be the only push of that script.

| Data length (bytes) | Script | Used by |
|---|---|---|
| 26 | `6a 1a ‖ data` | fee top-up |
| 46 | `6a 2e ‖ data` | prefund, deposit |
| 62 | `6a 3e ‖ data` | stake |
| 77 | `6a 4c 4d ‖ data` | checkpoint |

### Data leaf script

The data leaf script of a byte string `D` splits `D` into chunks of 520 bytes (the last one of at most 520 bytes). It is, for each chunk `c`, the minimal push of `c` followed by `OP_DROP` (`0x75`), and it ends with `OP_TRUE` (`0x51`). Example for a 120-byte `D`: `4c 78 ‖ D ‖ 75 51`.

**Remark.** Bitcoin relays only minimal pushes: `OP_PUSHBYTES_n`, `OP_PUSHDATA1` or `OP_PUSHDATA2` by length, and `OP_1` to `OP_16` or `OP_1NEGATE` for a one-byte chunk of value 1 to 16 or `0x81`, which push that byte.

### Commit-reveal pair

A message `D` too large for an OP_RETURN travels in a commit-reveal pair.

- The **commitment output** is output 0 of the commit transaction (in a checkpoint, output `p + W + 1`, [checkpoint](transactions/checkpoint.md#outputs)): a P2TR output with internal key `K_NUMS` and a single leaf (leaf version `0xc0`), the data leaf script `S` of `D`.
- The **reveal transaction** spends the commitment output through `S`. Its input witness is `[S, cb]`, where `cb` = `(0xc0 | parity)` ‖ `K_NUMS` (33 bytes), and `parity` is 1 if the y coordinate of the output key is odd and 0 otherwise (BIP341). Every control block in this specification starts with this byte. The input needs no signature.
- The commitment output's value covers the reveal's output and its fee: `v + fee_r(weight(reveal))`, where `v ≥ max(dust_r(P), dust(P), dust(destination))` is the reveal output's value and `P` the commitment scriptPubKey.
- Both transactions have version 2 and nLockTime 0. The reveal input has nSequence `0xffffffff`.

The monitor finds a message in the reveal: the data is the concatenation of the chunks that the pushes followed by `OP_DROP` push, in the leaf script executed by the input that spends the commitment output. Anyone who sees the commit transaction can build a reveal of the same data with other outputs, so the protocol takes only the data from the reveal.

### Configuration multisig

`MS(C)` for a configuration `C` of a subnet is the P2TR address with internal key `K_NUMS` and two leaves at depth 1 (leaf version `0xc0`):

- Leaf `M(C)`, the weighted multisig: `OP_0`, then for each key `pk_i` of `C` in ascending order of its 32 bytes `OP_SWAP OP_PUSHBYTES_32 pk_i OP_CHECKSIG OP_IF <w_i> OP_ADD OP_ENDIF`, then `<θ> OP_GREATERTHANOREQUAL`, where `w_i` is the key's power and `θ` the configuration's threshold ([power and thresholds](lifecycle.md#power-and-thresholds)). Bytes: `00` ‖ per key `7c 20 ‖ pk_i ‖ ac 63 ‖ num(w_i) ‖ 93 68` ‖ `num(θ) ‖ a2`, where `num(n)` is the minimal script-number push (`00` for 0, `51`–`60` for 1–16).
- Leaf `R`: `OP_RETURN OP_PUSHBYTES_20 txid20(subnet)` (`6a 14 ‖ txid20`), which makes the address unique per subnet.

A spend of `MS(C)` has, for every input, the witness `[s_n, …, s_1, M(C), cb]`. `s_i` is the 64-byte BIP340 signature of the i-th key in ascending order (so the greatest key's signature comes first) over the BIP341 script-path sighash of `M(C)` with `SIGHASH_DEFAULT`, or an empty element for a key that does not sign. `cb` = `(0xc0 | parity)` ‖ `K_NUMS` ‖ TapLeaf hash of `R` (65 bytes).

### Collateral script

The collateral of a validator with key `pk`, EOTS key `pk_eots` and timeout `T`, naming configuration `C`, is a P2TR output with internal key `K_NUMS` and three leaves (leaf version `0xc0`):

- **Unilateral-exit leaf** `U(pk, T)`: `<T> OP_CHECKLOCKTIMEVERIFY OP_DROP OP_PUSHBYTES_32 pk OP_CHECKSIG` (`num(T) ‖ b1 75 20 ‖ pk ‖ ac`). Only `pk` can spend it, and only in a transaction with nLockTime `≥ T`, that is from block `T + 1` on.
- **Slash leaf** `L(pk_eots, C)`: `OP_PUSHBYTES_32 pk_eots OP_CHECKSIGVERIFY`, then the weighted multisig `M(C)` (`20 ‖ pk_eots ‖ ad ‖ M(C)`). The [slash](transactions/slash.md) transaction spends it.
- **Cooperative-exit leaf** `X(pk, C)`: `OP_PUSHBYTES_32 pk OP_CHECKSIGVERIFY`, then `M(C)` (`20 ‖ pk ‖ ad ‖ M(C)`). A checkpoint spends the collateral through this leaf to [refresh](lifecycle.md#collateral-refresh) it.

`T` is a block height: `T < 500,000,000`. `pk_eots ≠ pk`, so the slash leaf and the cooperative-exit leaf differ.

The tree has `X(pk, C)` at depth 1, and `U(pk, T)` and `L(pk_eots, C)` at depth 2. Its Merkle root is the TapBranch hash of `TapLeaf(X)` and `TapBranch(TapLeaf(U), TapLeaf(L))`, where every TapBranch orders its two children as BIP341 does. The control block of a spend, after `(0xc0 | parity) ‖ K_NUMS`, holds:

| Leaf | Control block | Size (bytes) |
|---|---|---|
| `X(pk, C)` | `TapBranch(TapLeaf(U), TapLeaf(L))` | 65 |
| `U(pk, T)` | `TapLeaf(L)` ‖ `TapLeaf(X)` | 97 |
| `L(pk_eots, C)` | `TapLeaf(U)` ‖ `TapLeaf(X)` | 97 |

A spend through `U(pk, T)` has the witness `[σ, U(pk, T), cb]`. A spend through `L(pk_eots, C)` or `X(pk, C)` has the witness `[s_n, …, s_1, σ, leaf, cb]`, where `s_n, …, s_1` are as in the [multisig witness](#configuration-multisig) of `C`, and `σ` is the signature under `pk_eots` or `pk`, which the leaf checks first.

### Unbonding script

The unbonding output of a validator with key `pk` and EOTS key `pk_eots`, naming configuration `C`, is a P2TR output with internal key `K_NUMS` and two leaves at depth 1 (leaf version `0xc0`):

- **Unilateral-unbonding leaf** `B(pk)`: `<3K> OP_CHECKSEQUENCEVERIFY OP_DROP OP_PUSHBYTES_32 pk OP_CHECKSIG` (`num(3K) ‖ b2 75 20 ‖ pk ‖ ac`), with `K` the subnet's epoch length. Only `pk` can spend it, and only in a transaction of version `≥ 2` whose input has nSequence `3K`, that is from `3K` blocks after the block that includes the unbonding output on.
- **Slash leaf** `L(pk_eots, C)`, as in the [collateral script](#collateral-script).

The control block of a spend is `(0xc0 | parity) ‖ K_NUMS ‖` the TapLeaf hash of the other leaf (65 bytes). The witnesses are those of the collateral script: `[σ, B(pk), cb]` and `[s_n, …, s_1, σ, L(pk_eots, C), cb]`.

### EOTS nonce commitment

An EOTS nonce commitment `(start, count, root)` commits an EOTS key to one public nonce for each subnet height `h` with `start ≤ h < start + count`, where `start ≥ 1` and `count ≥ 1`. The public nonce `R_h` is the 32-byte x-only key of `k_h · G`, where `G` is the secp256k1 generator and `k_h` the validator's secret nonce. The validator chooses `k_h` so that `k_h · G` has an even y coordinate, as BIP340 requires of a signature nonce.

`root` is the RFC 6962 Merkle tree hash, with SHA-256, of the list `R_start, …, R_{start+count−1}`. A list of one element `x` hashes to `sha256(0x00 ‖ x)`. A list of `n > 1` elements splits after its first `k` elements, where `k` is the largest power of two smaller than `n`, and hashes to `sha256(0x01 ‖ left ‖ right)`. The proof of `R_h` is its RFC 6962 audit path in that tree, for the leaf index `h − start`.

In a message, an EOTS nonce commitment is the postcard tuple `(start: u64, count: u32, root: 32 bytes)`.
