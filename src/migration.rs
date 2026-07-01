// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! The versioned on-disk migration artifact format (issue #146).
//!
//! A migration file moves a whole sessiometer install between machines: every
//! managed account's restorable secret material plus the roster and app config.
//! This module owns only the **container format** — how those pieces are laid out,
//! versioned, and (in a later issue) wrapped in an encryption envelope. It does
//! NOT read the live keychain/config or write anything to disk; that wiring is the
//! export/import commands' job (later work items).
//!
//! # Layout
//!
//! The artifact is a single serde (JSON) document with three conceptual layers:
//!
//! ```text
//! preamble : magic + header.format_version   (peeked first — gates the version)
//! header   : format_version, encrypted flag, and — when encrypted — KDF + cipher
//!            parameters
//! body     : the payload — plaintext when `encrypted` is false, else opaque
//!            ciphertext that decrypts to the same serialized payload
//! ```
//!
//! The **preamble** (`magic` + `header.format_version`) is the stable prefix: it
//! must never move across format versions, because [`MigrationArtifact::from_bytes`]
//! peeks it *before* interpreting the body, so a future `format_version` is
//! rejected with a clear, typed [`Error::MigrationUnsupportedVersion`] instead of an
//! opaque body-parse failure.
//!
//! # Forward compatibility
//!
//! Two independent axes keep old files readable:
//!   - **`format_version`** gates the *structure* of the container. Bumping it means
//!     a change old readers cannot understand, so they reject it up front.
//!   - **KDF / cipher parameters travel IN the file** (the [`Header::kdf`] /
//!     [`Header::cipher`] slots), never hardcoded in the reader. A future change to,
//!     say, the Argon2id cost parameters is therefore just different *data* in the
//!     header — an old file still decrypts with the parameters it was written with,
//!     with NO `format_version` bump.
//!
//! # Independence from the encryption layer
//!
//! An unencrypted body is simply the `encrypted: false` case. This module defines
//! the `encrypted` flag and the parameter slots so the Argon2id / XChaCha20 envelope
//! (issue #147) can fill them; it deliberately implements **no** cryptography here —
//! [`Body::Ciphertext`] is opaque bytes.
//!
//! # Self-containment
//!
//! The carrier types below are decoupled from the live domain types
//! ([`crate::stash::StashedAccount`], [`crate::config::Config`],
//! [`crate::claude_state::OauthAccount`], [`crate::keychain::Credential`]): the
//! payload holds the config as its rendered `config.toml` text and each account's
//! two keychain halves as opaque bytes. Carrying the config verbatim is maximally
//! lossless — a new tunable can never be silently dropped by a stale mirror — and
//! keeps this module a pure format layer that merges cleanly alongside the
//! concurrent credential-subsystem work. The export/import wiring converts between
//! the live types and these carriers.
//!
//! # Secret hygiene
//!
//! [`ManagedAccount`] carries the account's bearer credential and its
//! `oauthAccount` identity block (which includes an email), so the secret-bearing
//! types deliberately derive **no** `Debug` (mirroring [`crate::claude_state`]), and
//! parse failures are redacted to a line/column position, never the surrounding
//! bytes (issue #15). The payload also carries **no** keychain ACL descriptor: on
//! import each stash item is re-written through the normal write path, which grants
//! a fresh local ACL (issue #146).

// The whole module is a not-yet-wired seam: issue #147 fills the encryption
// envelope and issues #148+ wire the export/import commands that construct and
// consume these types. Until then every item here is unused by the binary itself
// (main.rs only declares the module), exactly as main.rs frames every subsystem.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Magic marker distinguishing a sessiometer migration artifact from any other
/// JSON document. Part of the stable preamble.
const MAGIC: &str = "sessiometer-migration";

/// Current container format version. Gates the *structure* of the artifact:
/// [`MigrationArtifact::from_bytes`] rejects any other value up front. Bump only for
/// a structural change old readers cannot understand — parameter changes travel in
/// the header instead (see the module docs on forward compatibility).
const FORMAT_VERSION: u16 = 1;

/// A whole migration artifact: the stable magic, the structured header, and the
/// body (plaintext payload or opaque ciphertext).
///
/// No `Debug`: the body can carry bearer credentials and an `oauthAccount` email
/// block (see [`ManagedAccount`]).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MigrationArtifact {
    /// Stable marker; must equal [`MAGIC`]. Half of the peeked preamble.
    magic: String,
    /// Version + security metadata.
    header: Header,
    /// The payload, plaintext or ciphertext per [`Header::encrypted`].
    body: Body,
}

/// The structured header: the format version plus the security metadata. When the
/// artifact is encrypted the [`kdf`](Header::kdf) / [`cipher`](Header::cipher) slots
/// carry the parameters needed to derive the key and decrypt the body; a plaintext
/// artifact carries neither.
///
/// Non-secret (version + KDF/cipher parameters are not themselves secrets — the
/// salt/nonce are public inputs), so `Debug` is safe here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Header {
    /// Container structure version; the authoritative copy, and the second half of
    /// the peeked preamble.
    format_version: u16,
    /// Whether [`MigrationArtifact::body`] is ciphertext. Mandated by the format;
    /// cross-checked against the body variant on read
    /// ([`MigrationArtifact::validate`]).
    encrypted: bool,
    /// KDF parameters — present iff `encrypted` (issue #147 fills them). Travels in
    /// the file so a future parameter change still reads old files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kdf: Option<KdfParams>,
    /// Cipher parameters — present iff `encrypted` (issue #147 fills them).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cipher: Option<CipherParams>,
}

/// Key-derivation parameters carried in the header of an encrypted artifact.
///
/// The values live in the file, not in the reader, so issue #147 can change the
/// Argon2id cost parameters (or, via [`algorithm`](KdfParams::algorithm), the KDF
/// itself) without a `format_version` bump — an old file still derives its key from
/// the parameters it was written with. This module defines the slot; it performs no
/// key derivation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct KdfParams {
    /// KDF identifier, e.g. `"argon2id"`. A string (not an enum) so an
    /// unrecognized future algorithm is a decrypt-time decision (#147), not a parse
    /// failure that would stop the header from even being read.
    algorithm: String,
    /// Per-file random salt (hex-encoded on the wire).
    #[serde(with = "hex_bytes")]
    salt: Vec<u8>,
    /// Argon2 memory cost, in KiB.
    memory_kib: u32,
    /// Argon2 time cost, in iterations.
    iterations: u32,
    /// Argon2 parallelism, in lanes.
    parallelism: u32,
}

/// Cipher parameters carried in the header of an encrypted artifact. Like
/// [`KdfParams`], the values travel in the file; this module defines the slot and
/// performs no encryption.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CipherParams {
    /// Cipher identifier, e.g. `"xchacha20poly1305"`.
    algorithm: String,
    /// Per-file random nonce (hex-encoded on the wire).
    #[serde(with = "hex_bytes")]
    nonce: Vec<u8>,
}

/// The artifact body. `Plaintext` when [`Header::encrypted`] is false; `Ciphertext`
/// (opaque bytes that decrypt to the serialized [`Payload`]) when true. Issue #147
/// encrypts/decrypts between the two variants; this module treats the ciphertext as
/// opaque.
///
/// Adjacently tagged (`{"encoding": …, "data": …}`) so the variant is explicit on
/// the wire and cross-checkable against [`Header::encrypted`]. No `Debug`:
/// `Plaintext` carries secrets.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "data", rename_all = "snake_case")]
pub(crate) enum Body {
    /// The structured migration payload, in the clear.
    Plaintext(Payload),
    /// Opaque ciphertext (hex-encoded) that decrypts to the serialized [`Payload`].
    Ciphertext(#[serde(with = "hex_bytes")] Vec<u8>),
}

/// The migrated state: the whole `config.toml` (roster + tunables + refresh) plus
/// each managed account's secret material.
///
/// The roster travels *inside* [`config_toml`](Payload::config_toml) as its
/// `[[account]]` entries; [`accounts`](Payload::accounts) carries the matching
/// keychain secret material, keyed by `account_uuid`. No `Debug`: `accounts` carries
/// secrets.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Payload {
    /// The full `config.toml` content — roster (`[[account]]` entries) + tunables +
    /// refresh — carried verbatim as its canonical rendered text
    /// ([`crate::config::Config`]'s render output). Written as-is to the target's
    /// config path on import. Carrying it verbatim is lossless by construction: a
    /// tunable this module has never heard of still round-trips.
    config_toml: String,
    /// Per-account secret material, one entry per roster `[[account]]`, keyed by
    /// `account_uuid`.
    accounts: Vec<ManagedAccount>,
}

/// One managed account's restorable secret material — the two keychain-stashed
/// halves ([`crate::stash::StashedAccount`]): the raw `Claude Code-credentials`
/// bearer blob and the `oauthAccount` identity block.
///
/// Carries **no** keychain ACL descriptor: on import each item is re-written through
/// the normal stash write path, which grants a fresh local ACL (issue #146). No
/// `Debug`: both byte fields are secret material.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ManagedAccount {
    /// The roster key (`account_uuid`) this stash belongs to; matches a
    /// `[[account]]` entry in [`Payload::config_toml`].
    account_uuid: String,
    /// The raw `Claude Code-credentials` bearer blob, as opaque bytes (hex-encoded
    /// on the wire). Stored byte-identical to the canonical keychain item.
    #[serde(with = "hex_bytes")]
    credential: Vec<u8>,
    /// The account's `oauthAccount` identity block, as its canonical JSON bytes
    /// (hex-encoded on the wire). Carries the account's email, hence the redaction
    /// discipline.
    #[serde(with = "hex_bytes")]
    oauth_account: Vec<u8>,
}

impl MigrationArtifact {
    /// Build a version-current **plaintext** artifact (the `encrypted: false` case).
    pub(crate) fn plaintext(payload: Payload) -> Self {
        Self {
            magic: MAGIC.to_owned(),
            header: Header {
                format_version: FORMAT_VERSION,
                encrypted: false,
                kdf: None,
                cipher: None,
            },
            body: Body::Plaintext(payload),
        }
    }

    /// Build a version-current **encrypted** artifact — the slot issue #147 fills.
    /// Carries the opaque `ciphertext` plus the `kdf` / `cipher` parameters needed to
    /// derive the key and decrypt it. This module does no cryptography; the caller
    /// supplies already-encrypted bytes and the parameters that produced them.
    pub(crate) fn encrypted(ciphertext: Vec<u8>, kdf: KdfParams, cipher: CipherParams) -> Self {
        Self {
            magic: MAGIC.to_owned(),
            header: Header {
                format_version: FORMAT_VERSION,
                encrypted: true,
                kdf: Some(kdf),
                cipher: Some(cipher),
            },
            body: Body::Ciphertext(ciphertext),
        }
    }

    /// Serialize to the on-disk artifact bytes.
    ///
    /// Infallible: every field is a plain serde value (strings, hex-encoded byte
    /// vectors, integers, bools) with no map keys or floats, so JSON serialization
    /// cannot fail — mirroring [`crate::claude_state`]'s infallible re-serialization
    /// of an already-validated value.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializing a migration artifact cannot fail")
    }

    /// Parse and validate on-disk artifact bytes.
    ///
    /// Version-gates before interpreting the body: the stable preamble (magic +
    /// `format_version`) is peeked first, so a foreign document is rejected with
    /// [`Error::MigrationBadMagic`] and an unknown structure version with
    /// [`Error::MigrationUnsupportedVersion`] — never an opaque body-parse failure.
    /// Malformed JSON is redacted to a line/column position
    /// ([`Error::MigrationMalformed`]); the `encrypted` flag is cross-checked against
    /// the body ([`Error::MigrationInvalid`]).
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self> {
        // 1. Peek the stable preamble WITHOUT interpreting the body, so the version
        //    gate fires before any body-shape assumption. `Preamble` tolerates
        //    unknown/extra fields, so it still parses a future artifact this far —
        //    far enough to reject it by version.
        let preamble: Preamble = serde_json::from_slice(bytes).map_err(redact)?;
        if preamble.magic.as_deref() != Some(MAGIC) {
            return Err(Error::MigrationBadMagic);
        }
        if let Some(version) = preamble.header.and_then(|h| h.format_version) {
            if version != FORMAT_VERSION {
                return Err(Error::MigrationUnsupportedVersion {
                    found: version,
                    supported: FORMAT_VERSION,
                });
            }
        }

        // 2. Full parse (redacted on failure) + structural validation.
        let artifact: MigrationArtifact = serde_json::from_slice(bytes).map_err(redact)?;
        artifact.validate()?;
        Ok(artifact)
    }

    /// Structural invariants beyond what the type system enforces: the magic marker,
    /// and that the `encrypted` flag agrees with the body variant AND the presence of
    /// the KDF/cipher parameters. A plaintext artifact must NOT carry KDF/cipher
    /// parameters; an encrypted one MUST carry both.
    fn validate(&self) -> Result<()> {
        if self.magic != MAGIC {
            return Err(Error::MigrationBadMagic);
        }
        match (&self.body, self.header.encrypted) {
            (Body::Plaintext(_), false) => {
                if self.header.kdf.is_some() || self.header.cipher.is_some() {
                    return Err(Error::MigrationInvalid(
                        "a plaintext artifact must not carry KDF/cipher parameters",
                    ));
                }
            }
            (Body::Ciphertext(_), true) => {
                if self.header.kdf.is_none() || self.header.cipher.is_none() {
                    return Err(Error::MigrationInvalid(
                        "an encrypted artifact must carry both KDF and cipher parameters",
                    ));
                }
            }
            _ => {
                return Err(Error::MigrationInvalid(
                    "the header `encrypted` flag does not match the body",
                ));
            }
        }
        Ok(())
    }
}

/// The tolerant peek of the stable preamble (magic + `format_version`), parsed
/// before the full artifact so the version gate fires without interpreting the body.
/// Fields are optional and unknown/extra fields ignored, so it still parses a future
/// artifact — far enough to reject it by version.
#[derive(Deserialize)]
struct Preamble {
    #[serde(default)]
    magic: Option<String>,
    #[serde(default)]
    header: Option<PreambleHeader>,
}

/// The version-carrying slice of the header, for the [`Preamble`] peek.
#[derive(Deserialize)]
struct PreambleHeader {
    #[serde(default)]
    format_version: Option<u16>,
}

/// Map a `serde_json` error to the secret-free [`Error::MigrationMalformed`],
/// carrying only its line/column — never the surrounding bytes, which may hold an
/// account's credential / `oauthAccount` material (mirrors [`crate::claude_state`]
/// and issue #15 redaction).
fn redact(err: serde_json::Error) -> Error {
    Error::MigrationMalformed {
        line: err.line(),
        column: err.column(),
    }
}

/// Hex codec for the byte fields, as a serde `with` adapter, so opaque secret bytes
/// and the salt/nonce/ciphertext travel as pure-ASCII strings in the JSON container.
///
/// A private, self-contained copy of the same lowercase-hex scheme
/// [`crate::stash`] uses for keychain blobs — duplicated rather than shared to keep
/// this a pure format module that does not reach into the keychain code.
mod hex_bytes {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    /// Serialize bytes as a lowercase hex string.
    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&encode(bytes))
    }

    /// Deserialize a hex string back to bytes, rejecting a non-hex / odd-length
    /// value with a content-free error (only the position, added by the format,
    /// reaches the caller — see [`super::redact`]).
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let hex = String::deserialize(deserializer)?;
        decode(&hex).ok_or_else(|| D::Error::custom("invalid hex encoding"))
    }

    /// Encode bytes as lowercase, two-digits-per-byte hex (always pure ASCII).
    pub(super) fn encode(bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            // `from_digit(0..16, 16)` is infallible and yields `0-9a-f`.
            out.push(char::from_digit((b >> 4) as u32, 16).expect("high nibble < 16"));
            out.push(char::from_digit((b & 0x0f) as u32, 16).expect("low nibble < 16"));
        }
        out
    }

    /// Decode lowercase (or uppercase) hex, returning `None` for an odd length or a
    /// non-hex byte — i.e. a corrupted field.
    pub(super) fn decode(hex: &str) -> Option<Vec<u8>> {
        let bytes = hex.as_bytes();
        if !bytes.len().is_multiple_of(2) {
            return None;
        }
        let mut out = Vec::with_capacity(bytes.len() / 2);
        for pair in bytes.chunks_exact(2) {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            out.push((hi * 16 + lo) as u8);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative payload: a rendered `config.toml` fragment plus two managed
    /// accounts. One account's `oauthAccount` is deliberately NON-ASCII (accented +
    /// Cyrillic) — the case where a naive text round-trip of a keychain blob breaks
    /// — to prove the hex codec carries bytes `>= 0x80` byte-exact.
    fn sample_payload() -> Payload {
        Payload {
            config_toml: "[tunables]\npoll_secs = 300\n\n\
                          [[account]]\naccount_uuid = \"u-1\"\nlabel = \"work\"\nenabled = true\n\n\
                          [[account]]\naccount_uuid = \"u-2\"\nlabel = \"spare\"\nenabled = false\n"
                .to_owned(),
            accounts: vec![
                ManagedAccount {
                    account_uuid: "u-1".to_owned(),
                    credential: br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-EXAMPLE"}}"#.to_vec(),
                    oauth_account: "{\"accountUuid\":\"u-1\",\"displayName\":\"Cafe\u{301} \u{41e}\u{43b}\u{435}\u{43a}\u{441}i\u{439}\"}"
                        .as_bytes()
                        .to_vec(),
                },
                ManagedAccount {
                    account_uuid: "u-2".to_owned(),
                    credential: br#"{"claudeAiOauth":{"accessToken":"sk-ant-oat-TWO"}}"#.to_vec(),
                    oauth_account: br#"{"accountUuid":"u-2"}"#.to_vec(),
                },
            ],
        }
    }

    #[test]
    fn plaintext_round_trips_all_accounts_roster_and_config_with_no_loss() {
        // AC: serialize/deserialize round-trips all managed accounts (credential
        // blob + oauthAccount) + roster + config with no loss.
        let payload = sample_payload();
        let artifact = MigrationArtifact::plaintext(payload.clone());

        let bytes = artifact.to_bytes();
        let restored = MigrationArtifact::from_bytes(&bytes).unwrap();

        // No `Debug` on secret-bearing types → compare with `==`, not `assert_eq!`.
        assert!(
            restored == artifact,
            "the plaintext artifact must round-trip with no loss"
        );
        assert!(!restored.header.encrypted);

        // Spot-check that every carried piece survived structurally.
        match &restored.body {
            Body::Plaintext(p) => {
                assert_eq!(p.config_toml, payload.config_toml, "config (+ roster) lost");
                assert_eq!(p.accounts.len(), 2, "an account was lost");
                assert_eq!(p.accounts[0].account_uuid, "u-1");
                assert_eq!(
                    p.accounts[0].credential, payload.accounts[0].credential,
                    "credential blob altered"
                );
                assert_eq!(
                    p.accounts[0].oauth_account, payload.accounts[0].oauth_account,
                    "oauthAccount (non-ASCII) altered"
                );
                assert_eq!(
                    p.accounts[1].oauth_account,
                    payload.accounts[1].oauth_account
                );
            }
            Body::Ciphertext(_) => panic!("expected a plaintext body"),
        }
    }

    #[test]
    fn encrypted_shaped_artifact_round_trips_header_params_and_ciphertext() {
        // The ENCRYPTED shape is the slot issue #147 fills. No crypto here: the
        // ciphertext is opaque bytes. This proves the header's KDF + cipher
        // parameters and the ciphertext body travel IN the file and round-trip —
        // the forward-compatibility AC (parameters live in the file, not the
        // reader).
        let kdf = KdfParams {
            algorithm: "argon2id".to_owned(),
            salt: vec![0x00, 0x7f, 0x80, 0xff, 0x13, 0xa5],
            memory_kib: 65_536,
            iterations: 3,
            parallelism: 4,
        };
        let cipher = CipherParams {
            algorithm: "xchacha20poly1305".to_owned(),
            nonce: (0u8..24).collect(),
        };
        let ciphertext = vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x80, 0xff];

        let artifact =
            MigrationArtifact::encrypted(ciphertext.clone(), kdf.clone(), cipher.clone());
        let restored = MigrationArtifact::from_bytes(&artifact.to_bytes()).unwrap();

        assert!(
            restored == artifact,
            "the encrypted artifact must round-trip"
        );
        assert!(restored.header.encrypted);
        // The parameters travelled in the file (KdfParams/CipherParams are non-secret
        // → `Debug`, so `assert_eq!` is fine).
        assert_eq!(restored.header.kdf.as_ref(), Some(&kdf));
        assert_eq!(restored.header.cipher.as_ref(), Some(&cipher));
        match restored.body {
            Body::Ciphertext(c) => assert_eq!(c, ciphertext, "ciphertext altered"),
            Body::Plaintext(_) => panic!("expected a ciphertext body"),
        }
    }

    #[test]
    fn an_unknown_format_version_is_rejected_with_a_clear_error() {
        // AC: an unknown format_version is rejected with a clear error. Serialize a
        // valid v1 artifact, bump the on-disk version to a future value, and prove
        // the typed rejection carries both the found and supported versions — never
        // an opaque parse failure.
        let bytes = MigrationArtifact::plaintext(sample_payload()).to_bytes();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["header"]["format_version"] = serde_json::json!(999);
        let tampered = serde_json::to_vec(&value).unwrap();

        match MigrationArtifact::from_bytes(&tampered) {
            Err(Error::MigrationUnsupportedVersion { found, supported }) => {
                assert_eq!(found, 999);
                assert_eq!(supported, FORMAT_VERSION);
            }
            // `Error` has `Debug`; the `Ok` arm must not format the (no-`Debug`)
            // artifact.
            Err(other) => panic!("expected MigrationUnsupportedVersion, got {other:?}"),
            Ok(_) => panic!("expected MigrationUnsupportedVersion, got Ok"),
        }
    }

    #[test]
    fn a_foreign_or_missing_magic_is_rejected() {
        // A recognizable-version document that is NOT a sessiometer migration
        // artifact (wrong magic), and one with no magic at all, both reject as
        // BadMagic — the magic identifies the file class before the version gates it.
        assert!(matches!(
            MigrationArtifact::from_bytes(
                br#"{"magic":"something-else","header":{"format_version":1}}"#
            ),
            Err(Error::MigrationBadMagic)
        ));
        assert!(matches!(
            MigrationArtifact::from_bytes(br#"{"header":{"format_version":1}}"#),
            Err(Error::MigrationBadMagic)
        ));
    }

    #[test]
    fn an_inconsistent_encrypted_flag_is_rejected() {
        // A corrupt artifact whose `encrypted` flag disagrees with its body (flag
        // flipped to true while the body stays plaintext) is rejected structurally.
        let bytes = MigrationArtifact::plaintext(sample_payload()).to_bytes();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["header"]["encrypted"] = serde_json::json!(true);
        let tampered = serde_json::to_vec(&value).unwrap();

        assert!(matches!(
            MigrationArtifact::from_bytes(&tampered),
            Err(Error::MigrationInvalid(_))
        ));
    }

    #[test]
    fn a_managed_account_carries_no_keychain_acl_descriptor() {
        // AC: the payload carries NO keychain ACL descriptor (imported items get a
        // fresh local ACL via the normal write path). Prove the serialized
        // ManagedAccount exposes exactly the three intended keys and nothing
        // ACL-shaped.
        let bytes = MigrationArtifact::plaintext(sample_payload()).to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let account = value["body"]["data"]["accounts"][0]
            .as_object()
            .expect("a managed account serializes to an object");
        let mut keys: Vec<&str> = account.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["account_uuid", "credential", "oauth_account"]);

        // Belt-and-suspenders: no keychain-ACL vocabulary anywhere in the artifact.
        // (The exact-keys assertion above is the real guarantee; these guard against
        // an ACL field sneaking in under a future name.)
        let text = String::from_utf8(bytes).unwrap().to_lowercase();
        assert!(!text.contains("acl"), "no ACL descriptor may travel");
        assert!(!text.contains("partition"), "no partition list may travel");
        assert!(
            !text.contains("authorization"),
            "no ACL authorization may travel"
        );
    }

    #[test]
    fn validate_enforces_param_presence_matches_encryption() {
        // The other two `validate` branches (beyond the flag↔body mismatch above):
        // a plaintext artifact must NOT carry KDF/cipher parameters, and an
        // encrypted one MUST carry both. Both corruptions parse cleanly, then reject.
        let valid_kdf = serde_json::json!({
            "algorithm": "argon2id", "salt": "00", "memory_kib": 1,
            "iterations": 1, "parallelism": 1,
        });

        // Plaintext body carrying a stray KDF block → rejected.
        let plain = MigrationArtifact::plaintext(sample_payload()).to_bytes();
        let mut value: serde_json::Value = serde_json::from_slice(&plain).unwrap();
        value["header"]["kdf"] = valid_kdf;
        let tampered = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            MigrationArtifact::from_bytes(&tampered),
            Err(Error::MigrationInvalid(_))
        ));

        // Encrypted artifact with its KDF block removed → rejected.
        let kdf = KdfParams {
            algorithm: "argon2id".to_owned(),
            salt: vec![0x01, 0x02],
            memory_kib: 1,
            iterations: 1,
            parallelism: 1,
        };
        let cipher = CipherParams {
            algorithm: "xchacha20poly1305".to_owned(),
            nonce: vec![0x03],
        };
        let enc = MigrationArtifact::encrypted(vec![0xaa], kdf, cipher).to_bytes();
        let mut value: serde_json::Value = serde_json::from_slice(&enc).unwrap();
        value["header"]
            .as_object_mut()
            .unwrap()
            .remove("kdf")
            .expect("the encrypted artifact serialized a kdf block");
        let tampered = serde_json::to_vec(&value).unwrap();
        assert!(matches!(
            MigrationArtifact::from_bytes(&tampered),
            Err(Error::MigrationInvalid(_))
        ));
    }

    #[test]
    fn a_malformed_artifact_error_leaks_no_content() {
        // Not valid JSON, carrying a recognizable "secret" marker: the error must
        // carry only a position, never the surrounding bytes (issue #15 / the
        // claude_state redaction discipline).
        let poison = b"{ not json SECRET-sk-ant-oat-LEAK ";
        match MigrationArtifact::from_bytes(poison) {
            Err(Error::MigrationMalformed { line, column }) => {
                assert!(line >= 1 && column >= 1);
            }
            Err(other) => panic!("expected MigrationMalformed, got {other:?}"),
            Ok(_) => panic!("expected MigrationMalformed, got Ok"),
        }
        // `.err().unwrap()`, not `.unwrap_err()`: the latter needs `Debug` on the
        // Ok type, which the secret-bearing `MigrationArtifact` deliberately omits.
        let message = MigrationArtifact::from_bytes(poison)
            .err()
            .unwrap()
            .to_string();
        assert!(
            !message.contains("SECRET"),
            "error leaked content: {message}"
        );
        assert!(
            !message.contains("sk-ant"),
            "error leaked content: {message}"
        );
    }

    #[test]
    fn hex_round_trips_all_byte_values_and_rejects_bad_input() {
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        for sample in [b"".as_slice(), b"{\"a\":1}", &all_bytes] {
            let encoded = hex_bytes::encode(sample);
            assert!(encoded.is_ascii(), "hex output must be pure ASCII");
            assert_eq!(hex_bytes::decode(&encoded).as_deref(), Some(sample));
        }
        assert_eq!(hex_bytes::decode("abc"), None, "odd length rejected");
        assert_eq!(hex_bytes::decode("zz"), None, "non-hex rejected");
        assert_eq!(hex_bytes::decode("6X"), None, "one bad digit rejected");
        // Uppercase decodes (case-insensitive), matching the keychain stash codec.
        assert_eq!(hex_bytes::decode("4A").as_deref(), Some(b"\x4a".as_slice()));
    }
}
