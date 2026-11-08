//! Cryptographic helpers: password hashing, tokens, authenticated encryption and signatures.

use anyhow::{anyhow, bail, Result};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use data_encoding::HEXLOWER;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;
pub const B64_STD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

type HmacSha256 = Hmac<Sha256>;

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

pub fn random_token(n: usize) -> String {
    B64.encode(random_bytes(n))
}

pub fn sha256_hex(input: impl AsRef<[u8]>) -> String {
    HEXLOWER.encode(&Sha256::digest(input.as_ref()))
}

pub fn hmac_sha256_hex(secret: &[u8], message: &[u8]) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    HEXLOWER.encode(&mac.finalize().into_bytes())
}

pub fn hmac_sha256_b64(secret: &[u8], message: &[u8]) -> String {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    B64.encode(mac.finalize().into_bytes())
}

pub fn verify_hmac(secret: &[u8], message: &[u8], provided_hex: &str) -> bool {
    let expected = hmac_sha256_hex(secret, message);
    constant_time_eq(expected.as_bytes(), provided_hex.trim().as_bytes())
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.ct_eq(b).into()
}

pub fn hash_password(password: &str) -> Result<String> {
    validate_password(password)?;
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("password hashing failed: {e}"))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Password policy: length plus character variety. Argon2id handles storage security.
pub fn validate_password(password: &str) -> Result<()> {
    if password.len() < 10 {
        bail!("password must be at least 10 characters");
    }
    if password.len() > 256 {
        bail!("password must be at most 256 characters");
    }
    let has_letter = password.chars().any(|c| c.is_alphabetic());
    let has_other = password.chars().any(|c| !c.is_alphabetic());
    if !has_letter || !has_other {
        bail!("password must contain letters and at least one number or symbol");
    }
    Ok(())
}

/// Authenticated encryption for secrets at rest (TOTP seeds, node HMAC secrets).
#[derive(Clone)]
pub struct SecretBox {
    cipher: ChaCha20Poly1305,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(..)")
    }
}

impl SecretBox {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
        }
    }

    pub fn seal(&self, plaintext: &str) -> Result<String> {
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
            .map_err(|_| anyhow!("encryption failed"))?;
        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(B64_STD.encode(out))
    }

    pub fn open(&self, sealed: &str) -> Result<String> {
        let raw = B64_STD
            .decode(sealed.as_bytes())
            .map_err(|_| anyhow!("invalid sealed secret"))?;
        if raw.len() < 13 {
            bail!("sealed secret is too short");
        }
        let (nonce, ciphertext) = raw.split_at(12);
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(nonce), ciphertext)
            .map_err(|_| anyhow!("decryption failed"))?;
        String::from_utf8(plaintext).map_err(|_| anyhow!("sealed secret is not UTF-8"))
    }
}

/// Generate a `porgous_<prefix>_<secret>` API key and its lookup/storage halves.
pub struct ApiKeyMaterial {
    pub key: String,
    pub prefix: String,
    pub hash: String,
}

pub fn generate_api_key() -> ApiKeyMaterial {
    let prefix = random_token(6).to_lowercase().replace(['-', '_'], "x");
    let secret = random_token(32);
    let key = format!("porgous_{prefix}_{secret}");
    let hash = sha256_hex(key.as_bytes());
    ApiKeyMaterial { key, prefix, hash }
}

/// Constant-time verification of an API key against a stored SHA-256 hash.
pub fn verify_api_key(key: &str, stored_hash: &str) -> bool {
    let computed = sha256_hex(key.as_bytes());
    constant_time_eq(computed.as_bytes(), stored_hash.as_bytes())
}

pub fn api_key_prefix(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("porgous_")?;
    rest.split('_').next()
}

pub fn ed25519_public_key_bytes(value: &str) -> Result<[u8; 32]> {
    let value = value.trim();
    if let Ok(bytes) = B64_STD.decode(value) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes) {
            return Ok(key);
        }
    }
    if let Ok(bytes) = B64.decode(value) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes) {
            return Ok(key);
        }
    }
    if let Ok(bytes) = HEXLOWER.decode(value.as_bytes()) {
        if let Ok(key) = <[u8; 32]>::try_from(bytes) {
            return Ok(key);
        }
    }
    bail!("Ed25519 public key must be 32 bytes encoded as base64 or hex")
}

pub fn ed25519_verify(public_key: &str, message: &[u8], signature: &[u8]) -> bool {
    let Ok(key_bytes) = ed25519_public_key_bytes(public_key) else {
        return false;
    };
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(&key_bytes) else {
        return false;
    };
    let Ok(sig) = ed25519_dalek::Signature::from_slice(signature) else {
        return false;
    };
    key.verify_strict(message, &sig).is_ok()
}

pub fn ed25519_sign(seed: &[u8; 32], message: &[u8]) -> Vec<u8> {
    let signing = ed25519_dalek::SigningKey::from_bytes(seed);
    use ed25519_dalek::Signer;
    signing.sign(message).to_bytes().to_vec()
}

pub fn ed25519_public_from_seed(seed: &[u8; 32]) -> String {
    use ed25519_dalek::SigningKey;
    let signing = SigningKey::from_bytes(seed);
    B64_STD.encode(signing.verifying_key().to_bytes())
}

pub fn decode_signature(value: &[u8]) -> Option<Vec<u8>> {
    if value.len() == 64 {
        return Some(value.to_vec());
    }
    let text = String::from_utf8_lossy(value);
    let text = text.trim();
    for engine in [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(bytes) = engine.decode(text) {
            if bytes.len() == 64 {
                return Some(bytes);
            }
        }
    }
    HEXLOWER.decode(text.as_bytes()).ok().filter(|b| b.len() == 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let hash = hash_password("correct horse battery 1").unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert!(verify_password("correct horse battery 1", &hash));
        assert!(!verify_password("wrong password 1", &hash));
    }

    #[test]
    fn password_policy() {
        assert!(validate_password("short1").is_err());
        assert!(validate_password("onlylettershere").is_err());
        assert!(validate_password("longenough123").is_ok());
    }

    #[test]
    fn seal_roundtrip_and_tamper() {
        let sb = SecretBox::new(&[7u8; 32]);
        let sealed = sb.seal("totp-seed").unwrap();
        assert_eq!(sb.open(&sealed).unwrap(), "totp-seed");
        let mut raw = B64_STD.decode(sealed.as_bytes()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        assert!(sb.open(&B64_STD.encode(raw)).is_err());
    }

    #[test]
    fn api_keys_verify() {
        let material = generate_api_key();
        assert!(material.key.starts_with("porgous_"));
        assert!(verify_api_key(&material.key, &material.hash));
        assert!(!verify_api_key("porgous_bad_key", &material.hash));
        assert_eq!(api_key_prefix(&material.key), Some(material.prefix.as_str()));
    }

    #[test]
    fn ed25519_roundtrip() {
        let seed = [9u8; 32];
        let public = ed25519_public_from_seed(&seed);
        let sig = ed25519_sign(&seed, b"release");
        assert!(ed25519_verify(&public, b"release", &sig));
        assert!(!ed25519_verify(&public, b"tampered", &sig));
    }
}
