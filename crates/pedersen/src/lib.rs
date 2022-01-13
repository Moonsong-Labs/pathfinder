mod field;

use std::error::Error;

use crate::field::{
    FieldElement, FieldElementRepr, PEDERSEN_P0, PEDERSEN_P1, PEDERSEN_P2, PEDERSEN_P3, PEDERSEN_P4,
};

use bitvec::{order::Msb0, slice::BitSlice, view::BitView};
use ff::PrimeField;

/// The main hash code used by Starknet.
///
/// Contains 251 bits of data and is generated by the [pedersen_hash] function.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StarkHash([u8; 32]);
/// Error returned by [StarkHash::from_be_bytes] indicating that
/// more than the allowed 251 bits were set.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct OverflowError;

impl Error for OverflowError {}

impl std::fmt::Display for OverflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The StarkHash maximum was exceeded.")
    }
}

impl StarkHash {
    /// Returns the big-endian representation of this [StarkHash].
    pub fn to_be_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Creates a [StarkHash] from big-endian bytes.
    ///
    /// Returns [OverflowError] if more than 251 bits are set.
    pub fn from_be_bytes(bytes: [u8; 32]) -> Result<Self, OverflowError> {
        match bytes[0] {
            0..=0b0000_0111 => Ok(Self(bytes)),
            _ => Err(OverflowError),
        }
    }

    /// Returns a [StarkHash] equal to [0; 32].
    pub const fn zero() -> Self {
        StarkHash([0; 32])
    }

    /// Returns a bit view of the 251 least significant bits in MSB order.
    pub fn view_bits(&self) -> &BitSlice<Msb0, u8> {
        &self.0.view_bits()[5..]
    }

    /// Creates a [StarkHash] from up-to 251 bits.
    pub fn from_bits(bits: &BitSlice<Msb0, u8>) -> Result<Self, OverflowError> {
        if bits.len() > 251 {
            return Err(OverflowError);
        }

        let mut bytes = [0u8; 32];
        bytes.view_bits_mut::<Msb0>()[256 - bits.len()..].copy_from_bitslice(bits);

        Ok(Self(bytes))
    }
}

impl std::ops::Add for StarkHash {
    type Output = StarkHash;

    fn add(self, rhs: Self) -> Self::Output {
        let result = FieldElement::from(self) + FieldElement::from(rhs);
        StarkHash::from(result)
    }
}

/// Computes the [Starknet Pedersen hash] on `a` and `b`.
///
/// [Starknet Pedersen hash]: https://docs.starkware.co/starkex-v3/crypto/pedersen-hash-function
pub fn pedersen_hash(a: StarkHash, b: StarkHash) -> StarkHash {
    let mut result = PEDERSEN_P0.clone();
    let a = FieldElement::from(a).into_bits();
    let b = FieldElement::from(b).into_bits();

    // Add a_low * P1
    let tmp = PEDERSEN_P1.multiply(&a[..248]);
    result = result.add(&tmp);

    // Add a_high * P2
    let tmp = PEDERSEN_P2.multiply(&a[248..252]);
    result = result.add(&tmp);

    // Add b_low * P3
    let tmp = PEDERSEN_P3.multiply(&b[..248]);
    result = result.add(&tmp);

    // Add b_high * P4
    let tmp = PEDERSEN_P4.multiply(&b[248..252]);
    result = result.add(&tmp);

    // Return x-coordinate
    StarkHash::from(result.x)
}

impl From<StarkHash> for FieldElement {
    fn from(hash: StarkHash) -> Self {
        debug_assert_eq!(
            std::mem::size_of::<FieldElement>(),
            std::mem::size_of::<StarkHash>()
        );
        Self::from_repr(FieldElementRepr(hash.to_be_bytes())).unwrap()
    }
}

impl From<FieldElement> for StarkHash {
    fn from(fp: FieldElement) -> Self {
        debug_assert_eq!(
            std::mem::size_of::<FieldElement>(),
            std::mem::size_of::<StarkHash>()
        );
        // unwrap is safe because the FieldElement and StarkHash
        // should both be 251 bits only.
        StarkHash::from_be_bytes(fp.to_repr().0).unwrap()
    }
}

#[cfg(feature = "hex_str")]
impl StarkHash {
    /// A convenience function which parses a hex string into a [StarkHash].
    ///
    /// Supports both upper and lower case hex strings, as well as an
    /// optional "0x" prefix.
    pub fn from_hex_str(hex_str: &str) -> Result<StarkHash, HexParseError> {
        fn parse_hex_digit(digit: u8) -> Result<u8, HexParseError> {
            match digit {
                b'0'..=b'9' => Ok(digit - b'0'),
                b'A'..=b'F' => Ok(digit - b'A'),
                b'a'..=b'f' => Ok(digit - b'a'),
                other => Err(HexParseError::InvalidNibble(other)),
            }
        }

        let hex_str = hex_str.strip_prefix("0x").unwrap_or(hex_str);
        if hex_str.len() > 64 {
            return Err(HexParseError::Overflow);
        }

        let mut buf = [0u8; 32];

        // We want the result in big-endian so reverse iterate over each pair of nibbles.
        let chunks = hex_str.as_bytes().rchunks_exact(2);

        // Handle a possible odd nibble remaining nibble.
        let odd_nibble = chunks.remainder();
        if !odd_nibble.is_empty() {
            let full_bytes = hex_str.len() / 2;
            buf[31 - full_bytes] = parse_hex_digit(odd_nibble[0])?;
        }

        for (i, c) in chunks.enumerate() {
            // Indexing c[0] and c[1] are safe since chunk-size is 2.
            buf[31 - i] = parse_hex_digit(c[0])? << 4 | parse_hex_digit(c[1])?;
        }

        let hash = StarkHash::from_be_bytes(buf)?;
        Ok(hash)
    }
}

#[cfg(feature = "hex_str")]
#[derive(Debug)]
pub enum HexParseError {
    InvalidNibble(u8),
    Overflow,
}

#[cfg(feature = "hex_str")]
impl From<OverflowError> for HexParseError {
    fn from(_: OverflowError) -> Self {
        Self::Overflow
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitvec::bitvec;
    use pretty_assertions::assert_eq;

    #[test]
    fn view_bits() {
        let one = StarkHash::from_hex_str("1").unwrap();

        let one = one.view_bits().to_bitvec();

        let mut expected = bitvec![0; 251];
        expected.set(250, true);
        assert_eq!(one, expected);
    }

    #[test]
    fn bits_round_trip() {
        let mut bits = bitvec![Msb0, u8; 1; 251];
        bits.set(0, false);
        bits.set(1, false);
        bits.set(2, false);
        bits.set(3, false);
        bits.set(4, false);

        dbg!(bits.len());

        let res = StarkHash::from_bits(&bits).unwrap();

        let x = res.view_bits();
        let y = StarkHash::from_bits(x).unwrap();

        assert_eq!(res, y);
    }

    #[test]
    fn hash() {
        // Test vectors from https://github.com/starkware-libs/crypto-cpp/blob/master/src/starkware/crypto/pedersen_hash_test.cc
        let a = "03d937c035c878245caf64531a5756109c53068da139362728feb561405371cb";
        let b = "0208a0a10250e382e1e4bbe2880906c2791bf6275695e02fbbc6aeff9cd8b31a";
        let expected = "030e480bed5fe53fa909cc0f8c4d99b8f9f2c016be4c41e13a4848797979c662";

        fn parse_hex(str: &str) -> [u8; 32] {
            let mut buf = [0; 32];
            hex::decode_to_slice(str, &mut buf).unwrap();
            buf
        }

        let a = StarkHash::from_be_bytes(parse_hex(a)).unwrap();
        let b = StarkHash::from_be_bytes(parse_hex(b)).unwrap();
        let expected = StarkHash::from_be_bytes(parse_hex(expected)).unwrap();

        let hash = pedersen_hash(a, b);

        assert_eq!(hash, expected);
    }

    #[test]
    fn bytes_round_trip() {
        let original = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
            0x1C, 0x1D, 0x1E, 0x1F,
        ];
        let hash = StarkHash::from_be_bytes(original).unwrap();
        let bytes = hash.to_be_bytes();
        assert_eq!(bytes, original);
    }

    #[test]
    fn from_bytes_overflow() {
        // Set the 252nd bit (which is invalid).
        let mut bytes = [0; 32];
        bytes[0] = 0b0000_1000;
        assert_eq!(StarkHash::from_be_bytes(bytes), Err(OverflowError));
    }

    #[test]
    fn hash_field_round_trip() {
        let bytes = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B,
            0x1C, 0x1D, 0x1E, 0x1F,
        ];
        let original = StarkHash::from_be_bytes(bytes).unwrap();
        let fp = FieldElement::from(original);
        let hash = StarkHash::from(fp);
        assert_eq!(hash, original);
    }
}
