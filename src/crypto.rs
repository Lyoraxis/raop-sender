//! RAOP crypto: RSA-OAEP key exchange and AES-128-CBC packet encryption.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use rsa::{BigUint, Oaep, RsaPublicKey};
use sha1::Sha1;

const MODULUS_HEX: &str = "E7D744F2A2E2788B6C1F55A08EB70544A8FA7945AA8BE6C62CE5F51CBDD4DC6842FE3D1083DD2EDEC1BFD4252DC02E6F398BDF0E6148EA84855E2E442DA6D62664F674A1F304929ADE4F6893EF2DF6E711A8C77A0D91C9D980822E50D12922AFEA40EA9F0E14C0F76938C5F3882FC0323DD9FE55155F51BB5921C201629FD73352D5E2EFAABF9BA048D7B813A2B6767F6C3CCF1EB4CE673D037B0D2EA30C5FFFEB06F8D08ADDE409571A9C689FEF10728855DD8CFB9A8BEF5C8943EF3B5FAA15DDE698BEDDF3599603EB3E6F61372BB628F6559F599A78BF500687AA7F4976C0562D412956F8989E18A6355BD81597825E0FC875343EC782117625CDBF98447B";

pub fn public_key() -> RsaPublicKey {
    let n = BigUint::parse_bytes(MODULUS_HEX.as_bytes(), 16).expect("modulus");
    let e = BigUint::from(65537u32);
    RsaPublicKey::new(n, e).expect("rsa public key")
}

/// RSA-OAEP(SHA-1) wrap of the AES key, carried as `a=rsaaeskey`.
pub fn wrap_aes_key(aes_key: &[u8; 16]) -> Vec<u8> {
    let mut rng = rand::thread_rng();
    public_key()
        .encrypt(&mut rng, Oaep::new::<Sha1>(), aes_key)
        .expect("oaep encrypt")
}

/// Encrypt one audio payload for the wire.
///
/// Each packet is encrypted independently with AES-128-CBC and the same static
/// IV. Only the first `len & !0xF` bytes are encrypted: the receiver decrypts
/// exactly that prefix and copies the remainder through untouched, so there is
/// no length prefix and the payload keeps its original length.
pub fn encrypt_payload(payload: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
    let aes_len = payload.len() & !0xF;
    let mut out = payload.to_vec();
    if aes_len == 0 {
        return out;
    }

    let cipher = Aes128::new_from_slice(key).expect("aes key");
    let mut prev = *iv;
    for chunk in out[..aes_len].chunks_mut(16) {
        for i in 0..16 {
            chunk[i] ^= prev[i];
        }
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
        prev.copy_from_slice(chunk);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockDecrypt;

    fn cbc_decrypt(data: &[u8], key: &[u8; 16], iv: &[u8; 16]) -> Vec<u8> {
        let cipher = Aes128::new_from_slice(key).expect("aes key");
        let mut prev = *iv;
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            let mut block = GenericArray::clone_from_slice(chunk);
            let mut ciphertext = [0u8; 16];
            ciphertext.copy_from_slice(chunk);
            cipher.decrypt_block(&mut block);
            for i in 0..16 {
                block[i] ^= prev[i];
            }
            out.extend_from_slice(&block);
            prev = ciphertext;
        }
        out
    }

    #[test]
    fn only_the_aligned_prefix_is_encrypted() {
        let key = [7u8; 16];
        let iv = [9u8; 16];
        // 23 bytes: 16 encrypted, 7 left in plaintext.
        let payload: Vec<u8> = (0..23u8).collect();
        let wire = encrypt_payload(&payload, &key, &iv);

        assert_eq!(wire.len(), payload.len(), "framing must not change length");
        assert_ne!(&wire[..16], &payload[..16], "prefix must be encrypted");
        assert_eq!(&wire[16..], &payload[16..], "tail must be plaintext");

        let recovered = cbc_decrypt(&wire[..16], &key, &iv);
        assert_eq!(recovered, payload[..16]);
    }

    #[test]
    fn short_payloads_pass_through() {
        let key = [1u8; 16];
        let iv = [2u8; 16];
        let payload = vec![0xAAu8; 15];
        assert_eq!(encrypt_payload(&payload, &key, &iv), payload);
    }
}
