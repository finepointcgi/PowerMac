//! Key handling, the 24-bit "magic number" hash and the AES-256-CBC stream
//! cipher used on every MWB socket (Core/Encryption.cs).
//!
//! Stream format, independently in each direction of a TCP connection:
//!
//! ```text
//! [16-byte random salt][16-byte random IV]                        (cleartext)
//! AES-256-CBC(key = PBKDF2-HMAC-SHA512(MyKey(UTF-8), salt, 100000, 32 bytes), IV)
//!     over: [16 random bytes][package][package]...                (no padding ever
//!                                                                  emitted: every write is
//!                                                                  a multiple of 16 bytes)
//! ```
//!
//! The C# uses `PaddingMode.Zeros` with a `CryptoStream`; since all writes are
//! block multiples the stream is plain continuous CBC.

use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes256;
use rand::RngCore;
use sha2::{Digest, Sha512};

pub const BLOCK: usize = 16;
pub const SALT_SIZE: usize = 16;
pub const HEADER_SIZE: usize = SALT_SIZE + BLOCK;
pub const KDF_ITERATIONS: u32 = 100_000;
pub const KEY_LEN: usize = 32;

/// Normalize a user-entered key the way the MWB setup UI does
/// (`Regex.Replace(text, @"\s+", "")`): all whitespace is removed.
pub fn normalize_key(key: &str) -> String {
    key.chars().filter(|c| !c.is_whitespace()).collect()
}

/// `Encryption.IsKeyValid`: at least 16 characters.
pub fn is_key_valid(key: &str) -> bool {
    key.chars().count() >= 16
}

/// `Encryption.Get24BitHash`.
///
/// The key's first 32 UTF-16 code units are truncated to bytes into a zero-padded
/// 32-byte buffer, hashed with SHA-512, then re-hashed 50000 more times. Result:
/// `(h[0] << 23) + (h[1] << 16) + (h[63] << 8) + h[2]` (yes, `<< 23`, and the
/// arithmetic wraps like C# unchecked int).
pub fn get_24bit_hash(key: &str) -> u32 {
    if key.is_empty() {
        return 0;
    }
    let mut bytes = [0u8; 32];
    for (i, cu) in key.encode_utf16().take(32).enumerate() {
        bytes[i] = cu as u8;
    }
    let mut h = Sha512::digest(bytes);
    for _ in 0..50_000 {
        h = Sha512::digest(h);
    }
    ((h[0] as u32) << 23)
        .wrapping_add((h[1] as u32) << 16)
        .wrapping_add((h[63] as u32) << 8)
        .wrapping_add(h[2] as u32)
}

/// `Encryption.GenLegalKey`: PBKDF2-HMAC-SHA512 over the UTF-8 key.
pub fn derive_key(key: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha512>(key.as_bytes(), salt, KDF_ITERATIONS, &mut out);
    out
}

/// `Encryption.CreateRandomKey`: 16 chars cycling lower / upper / digit / symbol sets.
pub fn create_random_key() -> String {
    const SETS: [&str; 4] = [
        "abcdefghjkmnpqrstuvxyz",
        "ABCDEFGHJKMNPQRSTUVXYZ",
        "123456789",
        "~!@#$%^*()_-+=:;<,>.?/\\|[]",
    ];
    let mut rng = rand::rngs::OsRng;
    let mut key = String::new();
    while key.len() < 16 {
        for set in SETS {
            let mut b = [0u8; 1];
            rng.fill_bytes(&mut b);
            let set: Vec<char> = set.chars().collect();
            key.push(set[b[0] as usize % set.len()]);
            if key.len() >= 16 {
                break;
            }
        }
    }
    key
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

/// Continuous AES-256-CBC encryptor (no padding; input buffered to block size).
pub struct CbcEncryptor {
    cipher: Aes256,
    prev: [u8; BLOCK],
    pending: Vec<u8>,
}

impl CbcEncryptor {
    pub fn new(key: &[u8; KEY_LEN], iv: &[u8; BLOCK]) -> Self {
        CbcEncryptor { cipher: Aes256::new(GenericArray::from_slice(key)), prev: *iv, pending: Vec::new() }
    }

    /// Encrypt as many whole blocks as available; returns ciphertext.
    pub fn update(&mut self, data: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(data);
        let n = self.pending.len() / BLOCK * BLOCK;
        let mut out = Vec::with_capacity(n);
        for chunk in self.pending[..n].chunks(BLOCK) {
            let mut blk = [0u8; BLOCK];
            for i in 0..BLOCK {
                blk[i] = chunk[i] ^ self.prev[i];
            }
            let ga = GenericArray::from_mut_slice(&mut blk);
            self.cipher.encrypt_block(ga);
            self.prev = blk;
            out.extend_from_slice(&blk);
        }
        self.pending.drain(..n);
        out
    }
}

/// Continuous AES-256-CBC decryptor.
pub struct CbcDecryptor {
    cipher: Aes256,
    prev: [u8; BLOCK],
    pending: Vec<u8>,
}

impl CbcDecryptor {
    pub fn new(key: &[u8; KEY_LEN], iv: &[u8; BLOCK]) -> Self {
        CbcDecryptor { cipher: Aes256::new(GenericArray::from_slice(key)), prev: *iv, pending: Vec::new() }
    }

    pub fn update(&mut self, data: &[u8]) -> Vec<u8> {
        self.pending.extend_from_slice(data);
        let n = self.pending.len() / BLOCK * BLOCK;
        let mut out = Vec::with_capacity(n);
        for chunk in self.pending[..n].chunks(BLOCK) {
            let mut blk = [0u8; BLOCK];
            blk.copy_from_slice(chunk);
            let ct = blk;
            self.cipher.decrypt_block(GenericArray::from_mut_slice(&mut blk));
            for (b, p) in blk.iter_mut().zip(self.prev) {
                *b ^= p;
            }
            self.prev = ct;
            out.extend_from_slice(&blk);
        }
        self.pending.drain(..n);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbc_roundtrip_streaming() {
        let key = [7u8; 32];
        let iv = [9u8; 16];
        let mut e = CbcEncryptor::new(&key, &iv);
        let data: Vec<u8> = (0..96u8).collect();
        let mut ct = e.update(&data[..10]);
        ct.extend(e.update(&data[10..96]));
        assert_eq!(ct.len(), 96);
        let mut d = CbcDecryptor::new(&key, &iv);
        let mut pt = Vec::new();
        for c in ct.chunks(7) {
            pt.extend(d.update(c));
        }
        assert_eq!(pt, data);
    }

    #[test]
    fn random_key_shape() {
        for _ in 0..50 {
            let k = create_random_key();
            assert_eq!(k.chars().count(), 16);
            let c: Vec<char> = k.chars().collect();
            assert!(c[0].is_ascii_lowercase());
            assert!(c[1].is_ascii_uppercase());
            assert!(c[2].is_ascii_digit());
            assert!(!c[3].is_ascii_alphanumeric());
            assert!(!k.contains(['O', '0', 'o', 'l', 'I', 'i', 'w', 'W', '&', '"', '\'']));
        }
    }

    #[test]
    fn normalize() {
        assert_eq!(normalize_key(" abcd  EFGH\tijkl\nMNOP "), "abcdEFGHijklMNOP");
    }

    #[test]
    fn hash_empty() {
        assert_eq!(get_24bit_hash(""), 0);
    }
}
