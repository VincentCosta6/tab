//! The vault's cryptography, all of it in one place.
//!
//! - **Sealing** (records, files, the index): XChaCha20-Poly1305 with a
//!   random 24-byte nonce and associated data that names what's sealed,
//!   so the hub can't move a ciphertext from one record to another.
//! - **Wrapping** the vault key for a device: an ephemeral X25519 key
//!   agreement with the device's public key, HKDF-SHA256 over the shared
//!   secret and both public keys, then sealing as above (an HPKE-style
//!   sealed box).
//! - **Vouching**: HMAC-SHA256 under a vault key over something the hub
//!   could otherwise swap (a device's public key, the next vault key), so
//!   a device only trusts what someone holding the key wrote.
//! - **Recovery key**: 24 words (BIP-39, 256 bits). The words derive an
//!   X25519 key pair, so the vault key can be wrapped for "recovery" like
//!   for any device, and rewrapped on rotation without the words.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// A 256-bit symmetric key (a vault key, or one derived from it). Wiped
/// from memory when dropped.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Key([u8; 32]);

impl Key {
    pub fn random() -> Self {
        Self(random_bytes())
    }

    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// A key for one purpose, derived from this one, so the index and the
    /// records never share a key.
    pub fn derive(&self, purpose: &str) -> Key {
        let hk = Hkdf::<Sha256>::new(Some(b"tab vault v1"), &self.0);
        let mut out = [0u8; 32];
        hk.expand(purpose.as_bytes(), &mut out)
            .expect("32 bytes is a valid HKDF length");
        Key(out)
    }

    /// HMAC-SHA256 of `msg`, base64.
    pub fn vouch(&self, msg: &[u8]) -> String {
        B64.encode(self.mac(msg))
    }

    /// Whether `mac` is this key's [`Key::vouch`] for `msg` (constant time).
    pub fn vouches(&self, msg: &[u8], mac: &str) -> bool {
        let Ok(tag) = B64.decode(mac) else {
            return false;
        };
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.0).expect("any key length");
        m.update(msg);
        m.verify_slice(&tag).is_ok()
    }

    fn mac(&self, msg: &[u8]) -> Vec<u8> {
        let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&self.0).expect("any key length");
        m.update(msg);
        m.finalize().into_bytes().to_vec()
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key(…)")
    }
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the OS has a random number source");
    b
}

/// A sealed value as stored: `{nonce, ciphertext}` in base64, and the
/// vault key generation it's sealed under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sealed {
    pub key_id: String,
    pub nonce: String,
    pub ciphertext: String,
}

/// Seals `plain` under `key`, bound to `aad`.
pub fn seal(key: &Key, key_id: &str, aad: &[u8], plain: &[u8]) -> Sealed {
    let nonce: [u8; 24] = random_bytes();
    let ct = XChaCha20Poly1305::new(key.bytes().into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
        .expect("sealing in memory doesn't fail");
    Sealed {
        key_id: key_id.into(),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ct),
    }
}

/// Opens what [`seal`] sealed. Fails if anything was changed, or `aad`
/// differs (it was moved from another record).
pub fn open(key: &Key, aad: &[u8], s: &Sealed) -> anyhow::Result<Vec<u8>> {
    let nonce = B64.decode(&s.nonce)?;
    anyhow::ensure!(nonce.len() == 24, "bad nonce");
    let ct = B64.decode(&s.ciphertext)?;
    XChaCha20Poly1305::new(key.bytes().into())
        .decrypt(XNonce::from_slice(&nonce), Payload { msg: &ct, aad })
        .map_err(|_| anyhow::anyhow!("couldn't decrypt: wrong key, or it was changed"))
}

/// Seals bytes for the file store: nonce then ciphertext, no base64.
pub fn seal_bytes(key: &Key, aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let nonce: [u8; 24] = random_bytes();
    let mut out = nonce.to_vec();
    out.extend(
        XChaCha20Poly1305::new(key.bytes().into())
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad })
            .expect("sealing in memory doesn't fail"),
    );
    out
}

pub fn open_bytes(key: &Key, aad: &[u8], sealed: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(
        sealed.len() >= 24 + 16,
        "that file is too short to be sealed"
    );
    let (nonce, ct) = sealed.split_at(24);
    XChaCha20Poly1305::new(key.bytes().into())
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| anyhow::anyhow!("couldn't decrypt the file: wrong key, or it was changed"))
}

/// A device's (or the recovery key's) X25519 key pair.
pub struct KeyPair {
    secret: StaticSecret,
}

impl KeyPair {
    pub fn generate() -> Self {
        Self {
            secret: StaticSecret::from(random_bytes::<32>()),
        }
    }

    /// From what [`KeyPair::to_secret`] wrote.
    pub fn from_secret(b64: &str) -> anyhow::Result<Self> {
        let mut b: [u8; 32] = B64
            .decode(b64.trim())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("a device key is 32 bytes"))?;
        let secret = StaticSecret::from(b);
        b.zeroize();
        Ok(Self { secret })
    }

    pub fn to_secret(&self) -> String {
        B64.encode(self.secret.to_bytes())
    }

    pub fn public(&self) -> String {
        B64.encode(PublicKey::from(&self.secret).as_bytes())
    }
}

fn public_key(b64: &str) -> anyhow::Result<PublicKey> {
    let b: [u8; 32] = B64
        .decode(b64.trim())?
        .try_into()
        .map_err(|_| anyhow::anyhow!("a public key is 32 bytes"))?;
    Ok(PublicKey::from(b))
}

/// The vault key wrapped for one recipient.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Wrapped {
    /// The sender's ephemeral public key.
    pub ephemeral: String,
    pub nonce: String,
    pub ciphertext: String,
}

fn wrap_key(shared: &[u8; 32], ephemeral: &PublicKey, recipient: &PublicKey) -> Key {
    let mut ikm = shared.to_vec();
    ikm.extend(ephemeral.as_bytes());
    ikm.extend(recipient.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(b"tab vault wrap v1"), &ikm);
    ikm.zeroize();
    let mut out = [0u8; 32];
    hk.expand(b"vault key", &mut out).expect("valid length");
    Key(out)
}

/// What a wrap is bound to: which key generation, for whom.
fn wrap_aad(key_id: &str, recipient_id: &str) -> Vec<u8> {
    format!("tab vault key {key_id} for {recipient_id}").into_bytes()
}

/// Wraps `vault_key` (generation `key_id`) for the holder of `recipient`.
pub fn wrap(
    vault_key: &Key,
    key_id: &str,
    recipient_id: &str,
    recipient: &str,
) -> anyhow::Result<Wrapped> {
    let to = public_key(recipient)?;
    let eph = StaticSecret::from(random_bytes::<32>());
    let eph_pub = PublicKey::from(&eph);
    let shared = eph.diffie_hellman(&to);
    anyhow::ensure!(shared.was_contributory(), "that public key isn't usable");
    let k = wrap_key(shared.as_bytes(), &eph_pub, &to);
    let s = seal(
        &k,
        key_id,
        &wrap_aad(key_id, recipient_id),
        vault_key.bytes(),
    );
    Ok(Wrapped {
        ephemeral: B64.encode(eph_pub.as_bytes()),
        nonce: s.nonce,
        ciphertext: s.ciphertext,
    })
}

/// Unwraps a vault key wrapped for `me`.
pub fn unwrap(me: &KeyPair, key_id: &str, my_id: &str, w: &Wrapped) -> anyhow::Result<Key> {
    let eph = public_key(&w.ephemeral)?;
    let shared = me.secret.diffie_hellman(&eph);
    let k = wrap_key(shared.as_bytes(), &eph, &PublicKey::from(&me.secret));
    let mut plain = open(
        &k,
        &wrap_aad(key_id, my_id),
        &Sealed {
            key_id: key_id.into(),
            nonce: w.nonce.clone(),
            ciphertext: w.ciphertext.clone(),
        },
    )?;
    let b: [u8; 32] = plain
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("a vault key is 32 bytes"))?;
    plain.zeroize();
    Ok(Key(b))
}

/// The recovery key: 24 words, shown once.
pub struct Recovery {
    entropy: Key,
}

impl Recovery {
    pub fn generate() -> Self {
        Self {
            entropy: Key::random(),
        }
    }

    /// Reads the words back, forgiving case, spacing and line breaks.
    pub fn parse(words: &str) -> anyhow::Result<Self> {
        let norm = words
            .split(|c: char| c.is_whitespace() || c == ',' || c == '-')
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ");
        let m = bip39::Mnemonic::parse_in_normalized(bip39::Language::English, &norm)
            .map_err(|e| anyhow::anyhow!("that isn't a recovery key: {e}"))?;
        let b: [u8; 32] = m
            .to_entropy()
            .try_into()
            .map_err(|_| anyhow::anyhow!("a recovery key is 24 words"))?;
        Ok(Self { entropy: Key(b) })
    }

    pub fn words(&self) -> String {
        bip39::Mnemonic::from_entropy_in(bip39::Language::English, self.entropy.bytes())
            .expect("32 bytes is 24 words")
            .to_string()
    }

    /// The key pair the vault key is wrapped for as "recovery".
    pub fn key_pair(&self) -> KeyPair {
        let d = self.entropy.derive("recovery key pair");
        KeyPair {
            secret: StaticSecret::from(*d.bytes()),
        }
    }
}

/// The short code both devices show when one approves the other: the same
/// public key gives the same code, so the person can tell the hub didn't
/// swap in a key of its own.
pub fn device_code(device_id: &str, public: &str) -> String {
    let h = Sha256::digest(format!("tab vault device {device_id} {public}"));
    let n = u32::from_be_bytes([h[0], h[1], h[2], h[3]]) % 1_000_000;
    format!("{:03} {:03}", n / 1000, n % 1000)
}

/// Shows the end of a value and hides the rest: `•••• 6789`.
pub fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().filter(|c| !c.is_whitespace()).collect();
    if chars.len() <= 4 {
        return "••••".into();
    }
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("•••• {tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seals_and_refuses_tampering_or_moving() {
        let k = Key::random();
        let s = seal(&k, "k1", b"record a", b"12-3456789");
        assert_eq!(open(&k, b"record a", &s).unwrap(), b"12-3456789");
        // Moved to another record.
        assert!(open(&k, b"record b", &s).is_err());
        // Another key.
        assert!(open(&Key::random(), b"record a", &s).is_err());
        // Changed.
        let mut bad = s.clone();
        let mut ct = B64.decode(&bad.ciphertext).unwrap();
        ct[0] ^= 1;
        bad.ciphertext = B64.encode(ct);
        assert!(open(&k, b"record a", &bad).is_err());

        let f = seal_bytes(&k, b"file", b"pdf bytes");
        assert_eq!(open_bytes(&k, b"file", &f).unwrap(), b"pdf bytes");
        assert!(open_bytes(&k, b"other", &f).is_err());
    }

    #[test]
    fn wraps_for_one_device_only() {
        let vk = Key::random();
        let a = KeyPair::generate();
        let b = KeyPair::generate();
        let w = wrap(&vk, "k1", "dev-a", &a.public()).unwrap();
        assert_eq!(unwrap(&a, "k1", "dev-a", &w).unwrap().bytes(), vk.bytes());
        assert!(unwrap(&b, "k1", "dev-a", &w).is_err());
        // Bound to who it's for and which generation.
        assert!(unwrap(&a, "k1", "dev-b", &w).is_err());
        assert!(unwrap(&a, "k2", "dev-a", &w).is_err());
        // The secret survives the keychain round trip.
        let again = KeyPair::from_secret(&a.to_secret()).unwrap();
        assert_eq!(again.public(), a.public());
    }

    #[test]
    fn recovery_words_round_trip_to_the_same_key_pair() {
        let r = Recovery::generate();
        let words = r.words();
        assert_eq!(words.split(' ').count(), 24);
        let typed = words.to_uppercase().replace(' ', "\n  ");
        let back = Recovery::parse(&typed).unwrap();
        assert_eq!(back.key_pair().public(), r.key_pair().public());
        assert!(Recovery::parse("abandon abandon abandon").is_err());
        let vk = Key::random();
        let w = wrap(&vk, "k1", "recovery", &r.key_pair().public()).unwrap();
        assert_eq!(
            unwrap(&back.key_pair(), "k1", "recovery", &w)
                .unwrap()
                .bytes(),
            vk.bytes()
        );
    }

    #[test]
    fn vouches_codes_and_masks() {
        let k = Key::random();
        let mac = k.vouch(b"device x");
        assert!(k.vouches(b"device x", &mac));
        assert!(!k.vouches(b"device y", &mac));
        assert!(!Key::random().vouches(b"device x", &mac));
        assert_ne!(k.derive("index").bytes(), k.derive("records").bytes());

        let code = device_code("d1", "pub");
        assert_eq!(code, device_code("d1", "pub"));
        assert_ne!(code, device_code("d1", "other"));
        assert_eq!(code.len(), 7);

        assert_eq!(mask("12-3456789"), "•••• 6789");
        assert_eq!(mask("123"), "••••");
    }
}
