//! Protocol encryption and authentication primitives
//! (https://minecraft.wiki/w/Java_Edition_protocol/Encryption).

use aes::Aes128;
use bytes::Bytes;
use cfb8::cipher::inout::InOutBuf;
use cfb8::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use rsa::pkcs8::EncodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey, RsaPublicKey};
use sha1::{Digest, Sha1};

pub const SHARED_SECRET_LEN: usize = 16;
const RSA_BITS: usize = 1024;

/// The server's RSA keypair. Immutable after creation, so it is shared via `Arc`.
pub struct ServerKeys {
    private: RsaPrivateKey,
    public_der: Bytes,
}

impl ServerKeys {
    /// Generates a fresh RSA-1024 keypair. This is CPU-heavy; call it from a blocking context.
    pub fn generate() -> anyhow::Result<Self> {
        let private = RsaPrivateKey::new(&mut rand::rngs::OsRng, RSA_BITS)?;
        let public_der = RsaPublicKey::from(&private).to_public_key_der()?;
        Ok(Self {
            private,
            public_der: Bytes::copy_from_slice(public_der.as_bytes()),
        })
    }

    /// The public key as ASN.1 DER SubjectPublicKeyInfo, as sent in Encryption Request.
    pub fn public_der(&self) -> &Bytes {
        &self.public_der
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> rsa::Result<Vec<u8>> {
        self.private.decrypt(Pkcs1v15Encrypt, ciphertext)
    }
}

/// Stateful AES-128/CFB8 stream cipher for both directions of a connection.
/// The shared secret is used as both key and IV.
pub struct ProtocolCipher {
    encryptor: cfb8::Encryptor<Aes128>,
    decryptor: cfb8::Decryptor<Aes128>,
}

impl ProtocolCipher {
    pub fn new(shared_secret: &[u8; SHARED_SECRET_LEN]) -> Self {
        let key = shared_secret.into();
        Self {
            encryptor: cfb8::Encryptor::new(key, key),
            decryptor: cfb8::Decryptor::new(key, key),
        }
    }

    pub fn encrypt_in_place(&mut self, data: &mut [u8]) {
        let (blocks, _) = InOutBuf::from(data).into_chunks();
        self.encryptor.encrypt_blocks_inout_mut(blocks);
    }

    pub fn decrypt_in_place(&mut self, data: &mut [u8]) {
        let (blocks, _) = InOutBuf::from(data).into_chunks();
        self.decryptor.decrypt_blocks_inout_mut(blocks);
    }

    /// Separates the two directions so reads and writes can run on different tasks.
    pub fn split(self) -> (Encryptor, Decryptor) {
        (Encryptor(self.encryptor), Decryptor(self.decryptor))
    }
}

pub struct Encryptor(cfb8::Encryptor<Aes128>);

impl Encryptor {
    pub fn encrypt_in_place(&mut self, data: &mut [u8]) {
        let (blocks, _) = InOutBuf::from(data).into_chunks();
        self.0.encrypt_blocks_inout_mut(blocks);
    }
}

pub struct Decryptor(cfb8::Decryptor<Aes128>);

impl Decryptor {
    pub fn decrypt_in_place(&mut self, data: &mut [u8]) {
        let (blocks, _) = InOutBuf::from(data).into_chunks();
        self.0.decrypt_blocks_inout_mut(blocks);
    }
}

/// Computes the `serverId` hash sent to the session server: SHA-1 over the
/// server ID, shared secret, and public key, rendered as a signed
/// two's-complement hex number without leading zeros.
pub fn minecraft_hex_digest(server_id: &str, shared_secret: &[u8], public_der: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(server_id.as_bytes());
    hasher.update(shared_secret);
    hasher.update(public_der);
    signed_hex(hasher.finalize().into())
}

fn signed_hex(mut digest: [u8; 20]) -> String {
    let negative = digest[0] & 0x80 != 0;
    if negative {
        let mut carry = true;
        for byte in digest.iter_mut().rev() {
            *byte = !*byte;
            if carry {
                let (sum, overflow) = byte.overflowing_add(1);
                *byte = sum;
                carry = overflow;
            }
        }
    }

    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let trimmed = hex.trim_start_matches('0');
    let trimmed = if trimmed.is_empty() { "0" } else { trimmed };
    if negative {
        format!("-{trimmed}")
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs8::DecodePublicKey;

    fn name_digest(name: &str) -> String {
        let mut hasher = Sha1::new();
        hasher.update(name.as_bytes());
        signed_hex(hasher.finalize().into())
    }

    #[tuff_core::watchdog]
    #[test]
    fn hex_digest_matches_wiki_vectors() {
        assert_eq!(
            name_digest("Notch"),
            "4ed1f46bbe04bc756bcb17c0c7ce3e4632f06a48"
        );
        assert_eq!(
            name_digest("jeb_"),
            "-7c9d5b0044c130109a5d7b5fb5c317c02b4e28c1"
        );
        assert_eq!(
            name_digest("simon"),
            "88e16a1019277b15d58faf0541e11910eb756f6"
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn cfb8_is_stateful_across_chunks() {
        let secret = [7u8; SHARED_SECRET_LEN];
        let plaintext: Vec<u8> = (0..=255).collect();

        let mut whole = plaintext.clone();
        ProtocolCipher::new(&secret).encrypt_in_place(&mut whole);

        for chunk_size in [1, 7, 100] {
            let mut cipher = ProtocolCipher::new(&secret);
            let mut chunked = plaintext.clone();
            for chunk in chunked.chunks_mut(chunk_size) {
                cipher.encrypt_in_place(chunk);
            }
            assert_eq!(chunked, whole, "chunk size {chunk_size}");
        }

        let mut cipher = ProtocolCipher::new(&secret);
        for chunk in whole.chunks_mut(13) {
            cipher.decrypt_in_place(chunk);
        }
        assert_eq!(whole, plaintext);
    }

    #[tuff_core::watchdog]
    #[test]
    fn rsa_roundtrip_via_der_public_key() {
        let keys = ServerKeys::generate().unwrap();
        let public = RsaPublicKey::from_public_key_der(keys.public_der()).unwrap();
        let ciphertext = public
            .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, b"secret")
            .unwrap();
        assert_eq!(keys.decrypt(&ciphertext).unwrap(), b"secret");
    }
}
