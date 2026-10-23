//! EVM transaction building for Chain Signatures payouts.
//!
//! Everything here is pure except `keccak256`, which comes from the NEAR host. It covers RLP,
//! the EIP-1559 signing hash, the `r` value of the MPC signature, low-s normalisation, and the
//! signed raw transaction. The tests compare the encodings with `alloy-consensus` and recover
//! the signer with `k256`.

use near_sdk::env;

/// EIP-2718 type byte of an EIP-1559 transaction.
const TX_TYPE_EIP1559: u8 = 0x02;

/// Gas of a plain value transfer. Smaller gas limits can never be included.
const TRANSFER_GAS: u64 = 21_000;

/// secp256k1 group order `n`, big-endian.
const SECP256K1_N_HEX: &str = "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141";

/// `n / 2`. A low-s signature has `s <= n / 2` (EIP-2).
const SECP256K1_HALF_N_HEX: &str =
    "7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0";

/// A native-token transfer as an EIP-1559 (type 2) transaction with no calldata and an empty
/// access list. All fields are already parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Eip1559Transfer {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_priority_fee_per_gas: u128,
    pub max_fee_per_gas: u128,
    pub gas_limit: u64,
    pub to: [u8; 20],
    pub value: u128,
}

impl Eip1559Transfer {
    /// Checks the fields that the network would reject anyway, so a bad request fails here
    /// instead of after the MPC signature.
    pub fn validate(&self) -> Result<(), String> {
        if self.chain_id == 0 {
            return Err("chain_id must be positive".into());
        }
        if self.gas_limit < TRANSFER_GAS {
            return Err(format!("gas_limit must be at least {TRANSFER_GAS}"));
        }
        if self.max_fee_per_gas == 0 {
            return Err("max_fee_per_gas must be positive".into());
        }
        if self.max_priority_fee_per_gas > self.max_fee_per_gas {
            return Err("max_priority_fee_per_gas exceeds max_fee_per_gas".into());
        }
        Ok(())
    }

    /// The unsigned fields in their signed-transaction order, each RLP-encoded.
    fn fields(&self) -> Vec<Vec<u8>> {
        vec![
            rlp_uint(u128::from(self.chain_id)),
            rlp_uint(u128::from(self.nonce)),
            rlp_uint(self.max_priority_fee_per_gas),
            rlp_uint(self.max_fee_per_gas),
            rlp_uint(u128::from(self.gas_limit)),
            rlp_bytes(&self.to),
            rlp_uint(self.value),
            // data: empty for a plain transfer.
            rlp_bytes(&[]),
            // access_list: empty.
            rlp_list(&[]),
        ]
    }

    /// The 32-byte digest the MPC signs: `keccak256(0x02 || rlp([fields]))`.
    pub fn signing_hash(&self) -> [u8; 32] {
        let mut preimage = vec![TX_TYPE_EIP1559];
        preimage.extend(rlp_list(&self.fields()));
        keccak256(&preimage)
    }

    /// The signed raw transaction: `0x02 || rlp([fields, y_parity, r, s])`. `r` and `s` are
    /// 32-byte big-endian values, and `s` must already be low.
    pub fn signed_raw(&self, r: &[u8; 32], s: &[u8; 32], y_parity: u8) -> Vec<u8> {
        let mut fields = self.fields();
        fields.push(rlp_uint(u128::from(y_parity)));
        fields.push(rlp_bytes(strip_leading_zeros(r)));
        fields.push(rlp_bytes(strip_leading_zeros(s)));
        let mut raw = vec![TX_TYPE_EIP1559];
        raw.extend(rlp_list(&fields));
        raw
    }
}

/// Builds the signed raw transaction from the MPC output: the `r` coordinate of `big_r`, the
/// scalar `s`, and the recovery id. Normalises `s` to low-s, which flips the recovery id. The
/// MPC does not normalise (see docs/chain-signatures.md).
pub fn assemble_signed(
    transfer: &Eip1559Transfer,
    r: &[u8; 32],
    s: &[u8; 32],
    recovery_id: u8,
) -> Result<Vec<u8>, String> {
    if recovery_id > 1 {
        return Err(format!("recovery_id must be 0 or 1, got {recovery_id}"));
    }
    let n = secp256k1_n();
    if *s == [0u8; 32] || *s >= n {
        return Err("s is outside [1, n)".into());
    }
    let (s, y_parity) = if *s > secp256k1_half_n() {
        (sub_be(&n, s), recovery_id ^ 1)
    } else {
        (*s, recovery_id)
    };
    Ok(transfer.signed_raw(r, &s, y_parity))
}

/// The `r` value of an ECDSA signature from `big_r`, a 33-byte SEC1 compressed point. `r` is
/// the x-coordinate of R reduced mod `n`.
pub fn r_from_affine_point(point: &[u8]) -> Result<[u8; 32], String> {
    if point.len() != 33 || !(point[0] == 0x02 || point[0] == 0x03) {
        return Err("big_r must be a 33-byte compressed point".into());
    }
    let mut x = [0u8; 32];
    x.copy_from_slice(&point[1..]);
    // The field prime p is below 2n, so x < 2n and one subtraction reduces x mod n.
    let n = secp256k1_n();
    if x >= n {
        x = sub_be(&x, &n);
    }
    if x == [0u8; 32] {
        return Err("r is zero".into());
    }
    Ok(x)
}

/// Parses a 0x-prefixed 20-byte address.
pub fn parse_address(s: &str) -> Result<[u8; 20], String> {
    if !s.starts_with("0x") {
        return Err("address must start with 0x".into());
    }
    hex_decode(s)?
        .try_into()
        .map_err(|_| "address must be 20 bytes".to_string())
}

/// Decodes hex with an optional `0x` prefix.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let digits = s.strip_prefix("0x").unwrap_or(s);
    if digits.len() % 2 != 0 {
        return Err("hex string has odd length".into());
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| {
            digits
                .get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| "invalid hex digit".to_string())
        })
        .collect()
}

/// Lower-case hex with no prefix.
pub fn hex_plain(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Lower-case hex with a `0x` prefix.
pub fn hex_0x(bytes: &[u8]) -> String {
    format!("0x{}", hex_plain(bytes))
}

fn keccak256(data: &[u8]) -> [u8; 32] {
    env::keccak256_array(data)
}

fn secp256k1_n() -> [u8; 32] {
    hex_array(SECP256K1_N_HEX)
}

fn secp256k1_half_n() -> [u8; 32] {
    hex_array(SECP256K1_HALF_N_HEX)
}

/// Decodes a 32-byte constant. Only used with the constants above.
fn hex_array(s: &str) -> [u8; 32] {
    hex_decode(s)
        .expect("valid constant")
        .try_into()
        .expect("32-byte constant")
}

/// `a - b` for 32-byte big-endian values with `a >= b`.
fn sub_be(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut d = i16::from(a[i]) - i16::from(b[i]) - borrow;
        borrow = if d < 0 { 1 } else { 0 };
        if d < 0 {
            d += 256;
        }
        out[i] = d as u8;
    }
    out
}

/// Big-endian bytes with the leading zero bytes removed. Zero becomes the empty slice.
fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

/// RLP of a byte string.
pub fn rlp_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() == 1 && data[0] < 0x80 {
        return vec![data[0]];
    }
    let mut out = rlp_header(0x80, data.len());
    out.extend_from_slice(data);
    out
}

/// RLP of an unsigned integer: minimal big-endian bytes, with zero as the empty string.
pub fn rlp_uint(n: u128) -> Vec<u8> {
    rlp_bytes(strip_leading_zeros(&n.to_be_bytes()))
}

/// RLP of a list whose items are already RLP-encoded.
pub fn rlp_list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload: Vec<u8> = items.concat();
    let mut out = rlp_header(0xc0, payload.len());
    out.extend_from_slice(&payload);
    out
}

fn rlp_header(offset: u8, len: usize) -> Vec<u8> {
    if len <= 55 {
        // Lengths up to 55 fit in the prefix byte itself.
        return vec![offset + len as u8];
    }
    let len_bytes = strip_leading_zeros(&(len as u64).to_be_bytes()).to_vec();
    let mut out = vec![offset + 55 + len_bytes.len() as u8];
    out.extend(len_bytes);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::{Decodable2718, Encodable2718};
    use alloy_primitives::{Address, Bytes, PrimitiveSignature, TxKind, B256, U256};
    use k256::elliptic_curve::sec1::ToEncodedPoint;
    use k256::elliptic_curve::PrimeField;
    use k256::{FieldBytes, Scalar};

    fn transfer(chain_id: u64, nonce: u64, value: u128) -> Eip1559Transfer {
        Eip1559Transfer {
            chain_id,
            nonce,
            max_priority_fee_per_gas: 2_000_000_000,
            max_fee_per_gas: 30_000_000_000,
            gas_limit: 21_000,
            to: [0x11; 20],
            value,
        }
    }

    /// The same transaction as an `alloy-consensus` value, for comparison.
    fn alloy_tx(t: &Eip1559Transfer) -> TxEip1559 {
        TxEip1559 {
            chain_id: t.chain_id,
            nonce: t.nonce,
            gas_limit: t.gas_limit,
            max_fee_per_gas: t.max_fee_per_gas,
            max_priority_fee_per_gas: t.max_priority_fee_per_gas,
            to: TxKind::Call(Address::from(t.to)),
            value: U256::from(t.value),
            access_list: Default::default(),
            input: Bytes::new(),
        }
    }

    // ---- RLP, checked against the examples in the RLP specification ----

    #[test]
    fn rlp_spec_examples() {
        assert_eq!(rlp_bytes(b"dog"), vec![0x83, b'd', b'o', b'g']);
        assert_eq!(rlp_bytes(&[]), vec![0x80]);
        assert_eq!(rlp_bytes(&[0x0f]), vec![0x0f]);
        assert_eq!(rlp_bytes(&[0x80]), vec![0x81, 0x80]);
        assert_eq!(rlp_list(&[]), vec![0xc0]);
        assert_eq!(
            rlp_list(&[rlp_bytes(b"cat"), rlp_bytes(b"dog")]),
            vec![0xc8, 0x83, b'c', b'a', b't', 0x83, b'd', b'o', b'g']
        );
        assert_eq!(rlp_uint(0), vec![0x80]);
        assert_eq!(rlp_uint(15), vec![0x0f]);
        assert_eq!(rlp_uint(1024), vec![0x82, 0x04, 0x00]);
    }

    #[test]
    fn rlp_long_string_uses_length_of_length() {
        let data = [0xabu8; 56];
        let enc = rlp_bytes(&data);
        assert_eq!(&enc[..2], &[0xb8, 56]);
        assert_eq!(enc.len(), 58);
    }

    #[test]
    fn rlp_long_list_uses_length_of_length() {
        let item = rlp_bytes(&[0x01u8; 60]);
        let enc = rlp_list(std::slice::from_ref(&item));
        assert_eq!(enc[0], 0xf8);
        assert_eq!(enc[1] as usize, item.len());
    }

    // ---- Signing hash and signed encoding, checked against alloy-consensus ----

    #[test]
    fn signing_hash_matches_alloy() {
        let cases = [
            transfer(1, 0, 0),
            transfer(1, 0, 1),
            transfer(11_155_111, 7, 1_000_000_000_000_000_000),
            transfer(8453, 300, u128::MAX),
            Eip1559Transfer {
                max_priority_fee_per_gas: 0,
                max_fee_per_gas: u128::MAX,
                gas_limit: u64::MAX,
                nonce: u64::MAX,
                ..transfer(1, 0, 0)
            },
        ];
        for t in cases {
            let expected: B256 = alloy_tx(&t).signature_hash();
            assert_eq!(t.signing_hash(), expected.0, "signing hash for {t:?}");
        }
    }

    #[test]
    fn signed_raw_matches_alloy_encoding() {
        let r = [0x5a; 32];
        let s = [0x3c; 32];
        for parity in [0u8, 1] {
            for t in [transfer(1, 0, 0), transfer(11_155_111, 7, 123_456_789)] {
                let sig = PrimitiveSignature::new(
                    U256::from_be_bytes(r),
                    U256::from_be_bytes(s),
                    parity == 1,
                );
                let envelope = TxEnvelope::Eip1559(alloy_tx(&t).into_signed(sig));
                assert_eq!(t.signed_raw(&r, &s, parity), envelope.encoded_2718());
            }
        }
    }

    #[test]
    fn signed_raw_strips_leading_zeros_of_r_and_s() {
        // A small r and a 2-byte s have leading zero bytes. Compare with alloy, which encodes
        // minimal integers.
        let mut r = [0u8; 32];
        r[31] = 0x05;
        let mut s = [0u8; 32];
        s[30] = 0x01;
        let t = transfer(1, 0, 0);
        let sig = PrimitiveSignature::new(U256::from_be_bytes(r), U256::from_be_bytes(s), false);
        let envelope = TxEnvelope::Eip1559(alloy_tx(&t).into_signed(sig));
        assert_eq!(t.signed_raw(&r, &s, 0), envelope.encoded_2718());
    }

    // ---- MPC output handling ----

    #[test]
    fn r_is_x_coordinate_reduced_mod_n() {
        let mut point = [0u8; 33];
        point[0] = 0x02;
        point[1..].copy_from_slice(&[0x77; 32]);
        assert_eq!(r_from_affine_point(&point).unwrap(), [0x77; 32]);

        // x = n + 5 reduces to 5. Built from the group order so the test does not repeat the
        // constant in a different form.
        let n = secp256k1_n();
        let mut x = n;
        x[31] += 5; // n ends in 0x41, so no carry
        point[1..].copy_from_slice(&x);
        let mut expected = [0u8; 32];
        expected[31] = 5;
        assert_eq!(r_from_affine_point(&point).unwrap(), expected);
    }

    #[test]
    fn r_rejects_bad_points() {
        assert!(r_from_affine_point(&[0x02; 32]).is_err(), "wrong length");
        let mut point = [0x11u8; 33];
        point[0] = 0x04;
        assert!(r_from_affine_point(&point).is_err(), "uncompressed prefix");
        let mut zero = [0u8; 33];
        zero[0] = 0x02;
        assert!(r_from_affine_point(&zero).is_err(), "r = 0");
    }

    #[test]
    fn assemble_rejects_bad_scalars() {
        let t = transfer(1, 0, 0);
        let r = [0x01; 32];
        assert!(assemble_signed(&t, &r, &[0u8; 32], 0).is_err(), "s = 0");
        assert!(assemble_signed(&t, &r, &secp256k1_n(), 0).is_err(), "s = n");
        assert!(
            assemble_signed(&t, &r, &[0x02; 32], 2).is_err(),
            "recovery id 2"
        );
    }

    #[test]
    fn high_s_is_flipped_to_low_s_and_parity_changes() {
        let t = transfer(1, 0, 0);
        let r = [0x01; 32];
        let low = [0x20; 32]; // below n/2
        let high = sub_be(&secp256k1_n(), &low); // n - low, above n/2
        assert!(high > secp256k1_half_n());

        let from_low = assemble_signed(&t, &r, &low, 0).unwrap();
        let from_high = assemble_signed(&t, &r, &high, 1).unwrap();
        assert_eq!(
            from_low, from_high,
            "high s with flipped parity gives the same tx"
        );
        assert_eq!(from_low, t.signed_raw(&r, &low, 0));
    }

    #[test]
    fn sub_be_borrows_across_bytes() {
        let a = {
            let mut a = [0u8; 32];
            a[31] = 0x00;
            a[30] = 0x01;
            a
        };
        let mut b = [0u8; 32];
        b[31] = 0x01;
        let mut expected = [0u8; 32];
        expected[31] = 0xff; // 256 - 1
        assert_eq!(sub_be(&a, &b), expected);
    }

    #[test]
    fn hex_round_trip_and_address_parsing() {
        assert_eq!(hex_decode("0x0aff").unwrap(), vec![0x0a, 0xff]);
        assert_eq!(hex_decode("0aff").unwrap(), vec![0x0a, 0xff]);
        assert!(hex_decode("0xabc").is_err());
        assert!(hex_decode("0xzz").is_err());
        assert_eq!(hex_0x(&[0x0a, 0xff]), "0x0aff");
        assert!(parse_address(&format!("0x{}", "ab".repeat(20))).is_ok());
        assert!(parse_address(&format!("0x{}", "ab".repeat(19))).is_err());
        assert!(parse_address(&"ab".repeat(20)).is_err(), "missing prefix");
    }

    #[test]
    fn validate_checks_fee_and_gas_bounds() {
        assert!(transfer(1, 0, 0).validate().is_ok());
        assert!(transfer(0, 0, 0).validate().is_err());
        let low_gas = Eip1559Transfer {
            gas_limit: 20_999,
            ..transfer(1, 0, 0)
        };
        assert!(low_gas.validate().is_err());
        let inverted = Eip1559Transfer {
            max_priority_fee_per_gas: 2,
            max_fee_per_gas: 1,
            ..transfer(1, 0, 0)
        };
        assert!(inverted.validate().is_err());
    }

    // ---- Signing with k256 and recovering the signer ----

    /// Plays the MPC node for a known nonce `k`: returns `(big_r, s, recovery_id)` where
    /// `big_r` is the compressed point k*G and `s = k^-1 (z + r d)`, which can be high. The
    /// recovery id is the parity of R.y, as for a low-s signature.
    fn mpc_sign(secret: &Scalar, hash: &[u8; 32], k: &Scalar) -> ([u8; 33], [u8; 32], u8) {
        use k256::elliptic_curve::point::AffineCoordinates;
        use k256::ProjectivePoint;

        let big_r = (ProjectivePoint::GENERATOR * k).to_affine();
        // r = x(R) mod n. For this fixed nonce x(R) < n, so from_repr is exact.
        let r = Option::<Scalar>::from(Scalar::from_repr(big_r.x())).expect("x(R) < n");
        let z = Option::<Scalar>::from(Scalar::from_repr(FieldBytes::clone_from_slice(hash)))
            .expect("hash < n");
        let s = k.invert().unwrap() * (z + r * secret);
        let parity = u8::from(bool::from(big_r.y_is_odd()));

        let point_enc = big_r.to_encoded_point(true);
        let mut point = [0u8; 33];
        point.copy_from_slice(point_enc.as_bytes());
        (point, s.to_bytes().into(), parity)
    }

    #[test]
    fn mpc_style_output_assembles_to_a_tx_that_recovers_the_signer() {
        use k256::ecdsa::signature::hazmat::PrehashVerifier;
        use k256::ecdsa::{RecoveryId, Signature as EcdsaSig, SigningKey, VerifyingKey};
        let sk = SigningKey::from_bytes(&FieldBytes::clone_from_slice(&[0x42; 32])).unwrap();
        let secret: Scalar = *sk.as_nonzero_scalar().as_ref();
        let verifying: VerifyingKey = *sk.verifying_key();
        let address = {
            let enc = verifying.to_encoded_point(false);
            Address::from_slice(&alloy_primitives::keccak256(&enc.as_bytes()[1..])[12..])
        };

        for k in [0x1234_5678_9abc_def0u64, 0x0fed_cba9_8765_4321] {
            let t = transfer(11_155_111, 7, 1_000_000_000_000_000_000);
            let hash = t.signing_hash();
            let k = Scalar::from(k);
            let (big_r, s_raw, mpc_parity) = mpc_sign(&secret, &hash, &k);

            // The contract's path: r from big_r, then low-s normalisation and assembly.
            let r = r_from_affine_point(&big_r).unwrap();
            let raw = assemble_signed(&t, &r, &s_raw, mpc_parity).unwrap();

            // Independent check 1: alloy decodes the envelope and recovers the signer.
            let decoded = TxEnvelope::decode_2718(&mut raw.as_slice()).expect("valid envelope");
            assert_eq!(decoded.recover_signer().unwrap(), address);
            assert_eq!(decoded.signature_hash(), B256::from(hash));

            // Independent check 2: k256 verifies the low-s signature, and the parity recovers
            // the key. The expected low-s form is computed here, not by the code under test.
            let raw_sig = EcdsaSig::from_scalars(r, s_raw).unwrap();
            let (sig, parity) = match raw_sig.normalize_s() {
                Some(low) => (low, mpc_parity ^ 1),
                None => (raw_sig, mpc_parity),
            };
            verifying
                .verify_prehash(&hash, &sig)
                .expect("valid signature");
            let rec = VerifyingKey::recover_from_prehash(
                &hash,
                &sig,
                RecoveryId::from_byte(parity).unwrap(),
            )
            .unwrap();
            assert_eq!(rec, verifying);
        }
    }
}
