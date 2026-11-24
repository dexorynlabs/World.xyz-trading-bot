//! Solana wallet: key loading and in-place transaction signing.
//!
//! The venue returns a fully built, serialized `VersionedTransaction`. Instead
//! of deserializing it with the (heavy) solana-sdk, we patch our ed25519
//! signature straight into the wire bytes. This is a few microseconds and has
//! zero allocation beyond the base64 round trip.
//!
//! Wire layout:
//!   compact-u16 num_signatures | [64-byte signature; n] | message
//!   message = [0x80|version]? | header(3) | compact-u16 num_keys | [32-byte key; k] | ...
//! The first `header.num_required_signatures` keys are the signers, in the
//! same order as the signature slots.

use std::path::Path;

use anyhow::{bail, ensure, Context, Result};
use ed25519_dalek::{Signer as _, SigningKey};

pub struct Wallet {
    key: SigningKey,
    pubkey: [u8; 32],
    pubkey_b58: String,
}

impl Wallet {
    /// Accepts a base58 secret, a JSON byte array, or a path to a keypair JSON file.
    pub fn from_secret(input: &str) -> Result<Self> {
        let input = input.trim();
        let bytes: Vec<u8> = if input.starts_with('[') {
            serde_json::from_str(input).context("WALLET_PRIVATE_KEY: invalid JSON byte array")?
        } else if Path::new(input).is_file() {
            let raw = std::fs::read_to_string(input).context("reading keypair file")?;
            serde_json::from_str(raw.trim()).context("keypair file: invalid JSON byte array")?
        } else {
            bs58::decode(input)
                .into_vec()
                .context("WALLET_PRIVATE_KEY: invalid base58")?
        };

        let key = match bytes.len() {
            64 => {
                let arr: [u8; 64] = bytes.as_slice().try_into().expect("len checked");
                SigningKey::from_keypair_bytes(&arr)
                    .context("secret key does not match its embedded public key")?
            }
            32 => {
                let arr: [u8; 32] = bytes.as_slice().try_into().expect("len checked");
                SigningKey::from_bytes(&arr)
            }
            n => bail!("secret key must be 32 or 64 bytes, got {n}"),
        };
        Ok(Self::from_signing_key(key))
    }

    pub fn from_signing_key(key: SigningKey) -> Self {
        let pubkey = key.verifying_key().to_bytes();
        Self {
            pubkey_b58: bs58::encode(pubkey).into_string(),
            key,
            pubkey,
        }
    }

    pub fn pubkey(&self) -> &str {
        &self.pubkey_b58
    }

    /// Signs a serialized transaction in place. Returns the transaction id
    /// (base58 of signature slot 0, i.e. the fee payer's signature).
    pub fn sign_wire_tx(&self, wire: &mut [u8]) -> Result<String> {
        let (num_sigs, prefix) = decode_compact_u16(wire)?;
        let msg_start = prefix + num_sigs * 64;
        ensure!(wire.len() > msg_start, "transaction truncated before message");

        let (slot, signature) = {
            let msg = &wire[msg_start..];
            let mut p = usize::from(msg[0] & 0x80 != 0); // versioned prefix byte
            ensure!(msg.len() >= p + 3, "message header truncated");
            let num_required = msg[p] as usize;
            p += 3;
            let (num_keys, n) = decode_compact_u16(&msg[p..])?;
            p += n;
            ensure!(
                num_required == num_sigs && num_required <= num_keys,
                "signature count mismatch (slots={num_sigs}, required={num_required})"
            );
            ensure!(msg.len() >= p + num_keys * 32, "account keys truncated");

            let slot = (0..num_required)
                .find(|&i| msg[p + i * 32..p + i * 32 + 32] == self.pubkey)
                .with_context(|| {
                    format!("wallet {} is not a required signer of this tx", self.pubkey_b58)
                })?;
            (slot, self.key.sign(msg).to_bytes())
        };

        let at = prefix + slot * 64;
        wire[at..at + 64].copy_from_slice(&signature);
        Ok(bs58::encode(&wire[prefix..prefix + 64]).into_string())
    }
}

/// Solana "shortvec" length prefix. Returns (value, bytes consumed).
fn decode_compact_u16(b: &[u8]) -> Result<(usize, usize)> {
    let mut value = 0usize;
    for i in 0..3 {
        let byte = *b.get(i).context("compact-u16 truncated")?;
        value |= ((byte & 0x7f) as usize) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok((value, i + 1));
        }
    }
    bail!("compact-u16 overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    fn fake_v0_tx(signer: [u8; 32]) -> Vec<u8> {
        let mut msg = vec![0x80, 1, 0, 1, 2]; // v0, header(1 signer,0,1), 2 keys
        msg.extend_from_slice(&signer);
        msg.extend_from_slice(&[7u8; 32]);
        msg.extend_from_slice(&[9u8; 32]); // blockhash
        msg.extend_from_slice(&[0, 0]); // 0 instructions, 0 lookups
        let mut wire = vec![1u8];
        wire.extend_from_slice(&[0u8; 64]);
        wire.extend_from_slice(&msg);
        wire
    }

    #[test]
    fn signs_in_place_and_verifies() {
        let wallet = Wallet::from_signing_key(SigningKey::from_bytes(&[42u8; 32]));
        let mut wire = fake_v0_tx(wallet.pubkey);
        let txid = wallet.sign_wire_tx(&mut wire).unwrap();

        let sig = Signature::from_bytes(wire[1..65].try_into().unwrap());
        wallet.key.verifying_key().verify(&wire[65..], &sig).unwrap();
        assert_eq!(txid, bs58::encode(&wire[1..65]).into_string());
    }

    #[test]
    fn rejects_foreign_tx() {
        let wallet = Wallet::from_signing_key(SigningKey::from_bytes(&[42u8; 32]));
        let mut wire = fake_v0_tx([1u8; 32]);
        assert!(wallet.sign_wire_tx(&mut wire).is_err());
    }

    #[test]
    fn compact_u16() {
        assert_eq!(decode_compact_u16(&[0x05]).unwrap(), (5, 1));
        assert_eq!(decode_compact_u16(&[0x80, 0x01]).unwrap(), (128, 2));
        assert_eq!(decode_compact_u16(&[0xff, 0xff, 0x03]).unwrap(), (65535, 3));
    }
}
