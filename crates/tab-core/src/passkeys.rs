//! tab's own passkeys, for its in-app browser.
//!
//! macOS gives an embedded webview the system's passkeys only with an
//! entitlement Apple keeps for web browsers, so tab is its own authenticator
//! instead: a page's `navigator.credentials` calls reach
//! [`register`] and [`assert`], which keep P-256 keys in
//! the vault. The person adds a "tab" passkey on each site once; their
//! iCloud or 1Password passkeys aren't visible here.
//!
//! The caller passes the page's real origin (from the webview, never from
//! the page) and has confirmed it's the person (Touch ID) before either
//! call, so the authenticator data says the user was verified.

use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::vault::Vault;

/// ES256, the only algorithm tab's keys use.
const ES256: i64 = -7;
/// The vault entry holding every passkey.
const ENTRY: &str = "passkeys";

/// A stored passkey. `key` is the PKCS#8 private key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Passkey {
    pub id: String,
    pub rp_id: String,
    pub user_id: String,
    pub user_name: String,
    #[serde(default)]
    pub display_name: String,
    key: String,
    pub created: i64,
}

/// Who a passkey signs in as, for picking one.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Account {
    pub id: String,
    pub name: String,
}

/// Why a request was refused, as the page sees it: a `DOMException` name.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Refusal {
    pub name: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn not_allowed(message: impl Into<String>) -> Self {
        Self {
            name: "NotAllowedError",
            message: message.into(),
        }
    }
    fn security(message: impl Into<String>) -> Self {
        Self {
            name: "SecurityError",
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for Refusal {
    fn from(e: anyhow::Error) -> Self {
        Self::not_allowed(e.to_string())
    }
}

// What the page asked for, its buffers as base64url (see passkey_shim.js).

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateOptions {
    pub challenge: String,
    pub rp: Rp,
    pub user: User,
    #[serde(default)]
    pub pub_key_cred_params: Vec<Param>,
    #[serde(default)]
    pub exclude_credentials: Vec<CredentialRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rp {
    pub id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Param {
    pub alg: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CredentialRef {
    pub id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOptions {
    pub challenge: String,
    pub rp_id: Option<String>,
    #[serde(default)]
    pub allow_credentials: Vec<CredentialRef>,
}

/// The relying party a request names, checked against the page's origin.
/// `origin` is `scheme://host[:port]`.
pub fn rp_id(origin: &url::Url, asked: Option<&str>) -> Result<String, Refusal> {
    let host = origin
        .host_str()
        .ok_or_else(|| Refusal::security("the page has no host"))?
        .to_ascii_lowercase();
    let local = host == "localhost" || host.ends_with(".localhost");
    match origin.scheme() {
        "https" => {}
        "http" if local => {}
        _ => return Err(Refusal::security("passkeys need a secure page")),
    }
    let rp = asked
        .map(str::to_ascii_lowercase)
        .unwrap_or_else(|| host.clone());
    if host != rp && !host.ends_with(&format!(".{rp}")) {
        return Err(Refusal::security(format!(
            "{host} can't use passkeys for {rp}"
        )));
    }
    // A whole public suffix (github.io, co.uk) is shared by unrelated sites.
    if !local && psl::suffix_str(&rp) == Some(rp.as_str()) {
        return Err(Refusal::security(format!("{rp} is a public suffix")));
    }
    Ok(rp)
}

fn b64(s: &str) -> Result<Vec<u8>, Refusal> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|_| Refusal {
            name: "TypeError",
            message: "a buffer wasn't base64url".into(),
        })
}

fn client_data(kind: &str, challenge: &str, origin: &url::Url) -> Result<Vec<u8>, Refusal> {
    #[derive(Serialize)]
    struct ClientData<'a> {
        #[serde(rename = "type")]
        kind: &'a str,
        challenge: String,
        origin: String,
        #[serde(rename = "crossOrigin")]
        cross_origin: bool,
    }
    Ok(serde_json::to_vec(&ClientData {
        kind,
        challenge: URL_SAFE_NO_PAD.encode(b64(challenge)?),
        origin: origin.origin().ascii_serialization(),
        cross_origin: false,
    })
    .expect("plain struct"))
}

/// User present and user verified (Touch ID came first).
const UP_UV: u8 = 0x01 | 0x04;
/// Attested credential data follows.
const AT: u8 = 0x40;

fn auth_data(rp_id: &str, flags: u8, attested: Option<&[u8]>) -> Vec<u8> {
    let mut out = Sha256::digest(rp_id.as_bytes()).to_vec();
    out.push(flags);
    // No signature counter: zero, as synced passkeys do.
    out.extend(0u32.to_be_bytes());
    if let Some(a) = attested {
        out.extend(a);
    }
    out
}

/// The P-256 public key `0x04 || x || y` as SubjectPublicKeyInfo DER.
fn spki(point: &[u8]) -> Vec<u8> {
    const PREFIX: [u8; 26] = [
        0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08,
        0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
    ];
    [&PREFIX[..], point].concat()
}

/// Just enough CBOR for a COSE key and a "none" attestation object.
mod cbor {
    fn head(out: &mut Vec<u8>, major: u8, n: u64) {
        let m = major << 5;
        match n {
            0..=23 => out.push(m | n as u8),
            24..=0xff => out.extend([m | 24, n as u8]),
            0x100..=0xffff => {
                out.push(m | 25);
                out.extend((n as u16).to_be_bytes());
            }
            _ => {
                out.push(m | 26);
                out.extend((n as u32).to_be_bytes());
            }
        }
    }
    pub fn int(out: &mut Vec<u8>, v: i64) {
        if v >= 0 {
            head(out, 0, v as u64)
        } else {
            head(out, 1, (-1 - v) as u64)
        }
    }
    pub fn bytes(out: &mut Vec<u8>, b: &[u8]) {
        head(out, 2, b.len() as u64);
        out.extend(b);
    }
    pub fn text(out: &mut Vec<u8>, s: &str) {
        head(out, 3, s.len() as u64);
        out.extend(s.as_bytes());
    }
    pub fn map(out: &mut Vec<u8>, len: u64) {
        head(out, 5, len)
    }
}

/// The public key as a COSE_Key: EC2, ES256, P-256, x, y.
fn cose_key(point: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    cbor::map(&mut out, 5);
    for (k, v) in [(1, 2), (3, ES256), (-1, 1)] {
        cbor::int(&mut out, k);
        cbor::int(&mut out, v);
    }
    cbor::int(&mut out, -2);
    cbor::bytes(&mut out, &point[1..33]);
    cbor::int(&mut out, -3);
    cbor::bytes(&mut out, &point[33..65]);
    out
}

fn attestation_object(auth_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    cbor::map(&mut out, 3);
    cbor::text(&mut out, "fmt");
    cbor::text(&mut out, "none");
    cbor::text(&mut out, "attStmt");
    cbor::map(&mut out, 0);
    cbor::text(&mut out, "authData");
    cbor::bytes(&mut out, auth_data);
    out
}

/// What the page gets back; the shim turns it into a `PublicKeyCredential`.
/// Shaped like the spec's `toJSON()`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Credential {
    pub id: String,
    pub raw_id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub authenticator_attachment: &'static str,
    pub client_extension_results: serde_json::Value,
    pub response: serde_json::Value,
}

impl Credential {
    fn new(id: &str, response: serde_json::Value) -> Self {
        Self {
            id: id.into(),
            raw_id: id.into(),
            kind: "public-key",
            authenticator_attachment: "platform",
            client_extension_results: serde_json::json!({}),
            response,
        }
    }
}

/// Makes a passkey for `opts` at `origin`, adding it to `keys`. The caller
/// has checked [`check_register`] and confirmed it's the person.
pub fn register(
    keys: &mut Vec<Passkey>,
    origin: &url::Url,
    opts: &CreateOptions,
    now: i64,
) -> Result<Credential, Refusal> {
    let rp = check_register(keys, origin, opts)?;
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
        .map_err(|_| Refusal::not_allowed("couldn't make a key"))?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| Refusal::not_allowed("couldn't make a key"))?;
    let point = pair.public_key().as_ref();
    let mut raw = [0u8; 32];
    rng.fill(&mut raw)
        .map_err(|_| Refusal::not_allowed("couldn't make a key"))?;
    let id = URL_SAFE_NO_PAD.encode(raw);

    let mut attested = vec![0u8; 16]; // AAGUID: none
    attested.extend((raw.len() as u16).to_be_bytes());
    attested.extend(raw);
    attested.extend(cose_key(point));
    let data = auth_data(&rp, UP_UV | AT, Some(&attested));
    let client = client_data("webauthn.create", &opts.challenge, origin)?;

    // The user handle is kept as sent, re-encoded so it's canonical.
    let user_id = URL_SAFE_NO_PAD.encode(b64(&opts.user.id)?);
    // A site re-registering the same account replaces the old passkey.
    keys.retain(|k| !(k.rp_id == rp && k.user_id == user_id));
    keys.push(Passkey {
        id: id.clone(),
        rp_id: rp,
        user_id,
        user_name: opts.user.name.clone(),
        display_name: opts.user.display_name.clone(),
        key: STANDARD.encode(pkcs8.as_ref()),
        created: now,
    });
    let enc = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
    Ok(Credential::new(
        &id,
        serde_json::json!({
            "clientDataJSON": enc(&client),
            "attestationObject": enc(&attestation_object(&data)),
            "authenticatorData": enc(&data),
            "publicKey": enc(&spki(point)),
            "publicKeyAlgorithm": ES256,
            "transports": ["internal"],
        }),
    ))
}

/// The relying party a registration would be for, or why it can't be made.
pub fn check_register(
    keys: &[Passkey],
    origin: &url::Url,
    opts: &CreateOptions,
) -> Result<String, Refusal> {
    let rp = rp_id(origin, opts.rp.id.as_deref())?;
    if !opts.pub_key_cred_params.is_empty()
        && !opts.pub_key_cred_params.iter().any(|p| p.alg == ES256)
    {
        return Err(Refusal {
            name: "NotSupportedError",
            message: "tab's passkeys use ES256, which the site didn't offer".into(),
        });
    }
    if opts.exclude_credentials.iter().any(|c| {
        keys.iter()
            .any(|k| k.rp_id == rp && k.id == c.id.trim_end_matches('='))
    }) {
        return Err(Refusal {
            name: "InvalidStateError",
            message: "tab already has a passkey for this account".into(),
        });
    }
    Ok(rp)
}

/// The passkeys that could answer `opts` at `origin`.
pub fn candidates(
    keys: &[Passkey],
    origin: &url::Url,
    opts: &GetOptions,
) -> Result<(String, Vec<Account>), Refusal> {
    let rp = rp_id(origin, opts.rp_id.as_deref())?;
    let accounts = keys
        .iter()
        .filter(|k| k.rp_id == rp)
        .filter(|k| {
            opts.allow_credentials.is_empty()
                || opts
                    .allow_credentials
                    .iter()
                    .any(|c| c.id.trim_end_matches('=') == k.id)
        })
        .map(|k| Account {
            id: k.id.clone(),
            name: if k.user_name.is_empty() {
                k.display_name.clone()
            } else {
                k.user_name.clone()
            },
        })
        .collect();
    Ok((rp, accounts))
}

/// Signs in with passkey `id`. The caller got it from [`candidates`] and
/// has confirmed it's the person.
pub fn assert(
    keys: &[Passkey],
    origin: &url::Url,
    opts: &GetOptions,
    id: &str,
) -> Result<Credential, Refusal> {
    let (rp, accounts) = candidates(keys, origin, opts)?;
    if !accounts.iter().any(|a| a.id == id) {
        return Err(Refusal::not_allowed("that passkey isn't for this site"));
    }
    let key = keys.iter().find(|k| k.id == id).expect("a candidate");
    let pkcs8 = STANDARD
        .decode(&key.key)
        .map_err(|_| Refusal::not_allowed("the passkey is damaged"))?;
    let rng = SystemRandom::new();
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng)
        .map_err(|_| Refusal::not_allowed("the passkey is damaged"))?;
    let data = auth_data(&rp, UP_UV, None);
    let client = client_data("webauthn.get", &opts.challenge, origin)?;
    let signed = [&data[..], &Sha256::digest(&client)].concat();
    let sig = pair
        .sign(&rng, &signed)
        .map_err(|_| Refusal::not_allowed("couldn't sign"))?;
    let enc = |b: &[u8]| URL_SAFE_NO_PAD.encode(b);
    Ok(Credential::new(
        id,
        serde_json::json!({
            "clientDataJSON": enc(&client),
            "authenticatorData": enc(&data),
            "signature": enc(sig.as_ref()),
            "userHandle": key.user_id,
        }),
    ))
}

/// The passkeys, kept as one vault entry.
pub struct Passkeys {
    vault: Vault,
    lock: Mutex<()>,
}

impl Passkeys {
    pub fn new(dir: std::path::PathBuf) -> Self {
        Self {
            vault: Vault::named(dir, "tab passkeys"),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> anyhow::Result<Vec<Passkey>> {
        Ok(match self.vault.try_get(ENTRY)? {
            Some(s) => serde_json::from_str(&s)?,
            None => Vec::new(),
        })
    }

    /// Reads the passkeys, lets `f` change them, and saves them if it did.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&mut Vec<Passkey>) -> Result<T, Refusal>,
    ) -> Result<T, Refusal> {
        let _held = self.lock.lock().unwrap();
        let mut keys = self.load()?;
        let before = serde_json::to_string(&keys).map_err(anyhow::Error::from)?;
        let out = f(&mut keys)?;
        let after = serde_json::to_string(&keys).map_err(anyhow::Error::from)?;
        if after != before {
            self.vault.set(ENTRY, &after)?;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_ASN1, UnparsedPublicKey};

    fn url(s: &str) -> url::Url {
        s.parse().unwrap()
    }

    fn create(rp: Option<&str>, user: &str) -> CreateOptions {
        serde_json::from_value(serde_json::json!({
            "challenge": "AQID",
            "rp": { "id": rp, "name": "GitHub" },
            "user": { "id": URL_SAFE_NO_PAD.encode(user), "name": user, "displayName": user },
            "pubKeyCredParams": [{ "type": "public-key", "alg": -7 }, { "type": "public-key", "alg": -257 }],
        }))
        .unwrap()
    }

    fn get(allow: &[&str]) -> GetOptions {
        serde_json::from_value(serde_json::json!({
            "challenge": "BAUG",
            "rpId": "github.com",
            "allowCredentials": allow.iter().map(|id| serde_json::json!({ "id": id })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn checks_the_relying_party_against_the_page() {
        assert_eq!(
            rp_id(&url("https://github.com/login"), None).unwrap(),
            "github.com"
        );
        assert_eq!(
            rp_id(&url("https://gist.github.com/"), Some("github.com")).unwrap(),
            "github.com"
        );
        assert_eq!(
            rp_id(&url("https://github.com/"), Some("evil.com"))
                .unwrap_err()
                .name,
            "SecurityError"
        );
        assert_eq!(
            rp_id(&url("https://notgithub.com/"), Some("github.com"))
                .unwrap_err()
                .name,
            "SecurityError"
        );
        assert_eq!(
            rp_id(&url("https://me.github.io/"), Some("github.io"))
                .unwrap_err()
                .name,
            "SecurityError"
        );
        assert_eq!(
            rp_id(&url("http://github.com/"), None).unwrap_err().name,
            "SecurityError"
        );
        assert_eq!(
            rp_id(&url("http://localhost:3000/"), None).unwrap(),
            "localhost"
        );
    }

    #[test]
    fn registers_then_signs_in_with_a_verifiable_signature() {
        let mut keys = Vec::new();
        let origin = url("https://github.com/settings/security");
        let made = register(&mut keys, &origin, &create(None, "vincent"), 1).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].rp_id, "github.com");

        // The attestation carries the key the site will verify with.
        let att = URL_SAFE_NO_PAD
            .decode(made.response["attestationObject"].as_str().unwrap())
            .unwrap();
        assert!(att.starts_with(&[0xa3, 0x63, b'f', b'm', b't', 0x64, b'n', b'o', b'n', b'e']));
        let data = URL_SAFE_NO_PAD
            .decode(made.response["authenticatorData"].as_str().unwrap())
            .unwrap();
        assert_eq!(&data[..32], Sha256::digest(b"github.com").as_slice());
        assert_eq!(data[32], UP_UV | AT);
        let client: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(made.response["clientDataJSON"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(client["type"], "webauthn.create");
        assert_eq!(client["origin"], "https://github.com");
        assert_eq!(client["challenge"], "AQID");

        let login = url("https://github.com/login");
        let (_, found) = candidates(&keys, &login, &get(&[])).unwrap();
        assert_eq!(
            found,
            vec![Account {
                id: made.id.clone(),
                name: "vincent".into()
            }]
        );
        let signed = assert(&keys, &login, &get(&[&made.id]), &made.id).unwrap();

        let spki_der = URL_SAFE_NO_PAD
            .decode(made.response["publicKey"].as_str().unwrap())
            .unwrap();
        let point = &spki_der[26..];
        let data = URL_SAFE_NO_PAD
            .decode(signed.response["authenticatorData"].as_str().unwrap())
            .unwrap();
        let client = URL_SAFE_NO_PAD
            .decode(signed.response["clientDataJSON"].as_str().unwrap())
            .unwrap();
        let sig = URL_SAFE_NO_PAD
            .decode(signed.response["signature"].as_str().unwrap())
            .unwrap();
        let msg = [&data[..], &Sha256::digest(&client)].concat();
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, point)
            .verify(&msg, &sig)
            .unwrap();
        assert_eq!(
            signed.response["userHandle"],
            URL_SAFE_NO_PAD.encode("vincent")
        );
    }

    #[test]
    fn keeps_passkeys_to_their_site() {
        let mut keys = Vec::new();
        let made = register(
            &mut keys,
            &url("https://github.com/"),
            &create(None, "v"),
            1,
        )
        .unwrap();
        let elsewhere: GetOptions =
            serde_json::from_value(serde_json::json!({ "challenge": "AA" })).unwrap();
        let (rp, found) = candidates(&keys, &url("https://gitlab.com/"), &elsewhere).unwrap();
        assert_eq!(rp, "gitlab.com");
        assert!(found.is_empty());
        assert!(assert(&keys, &url("https://gitlab.com/"), &elsewhere, &made.id).is_err());
        // Not in the site's allow list: not offered.
        assert!(
            candidates(&keys, &url("https://github.com/"), &get(&["other"]))
                .unwrap()
                .1
                .is_empty()
        );
    }

    #[test]
    fn refuses_duplicates_and_unsupported_algorithms() {
        let mut keys = Vec::new();
        let origin = url("https://github.com/");
        let made = register(&mut keys, &origin, &create(None, "v"), 1).unwrap();
        let mut again = create(None, "v");
        again.exclude_credentials = vec![CredentialRef {
            id: made.id.clone(),
        }];
        assert_eq!(
            check_register(&keys, &origin, &again).unwrap_err().name,
            "InvalidStateError"
        );

        let mut rsa = create(None, "w");
        rsa.pub_key_cred_params = vec![Param { alg: -257 }];
        assert_eq!(
            check_register(&keys, &origin, &rsa).unwrap_err().name,
            "NotSupportedError"
        );

        // The same account registered again replaces its passkey.
        register(&mut keys, &origin, &create(None, "v"), 2).unwrap();
        assert_eq!(keys.len(), 1);
    }
}
