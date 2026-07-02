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

use std::io::BufRead;
use std::path::Path;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload as AeadPayload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

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

impl Payload {
    /// Assemble a payload from the gathered live state — the rendered `config.toml`
    /// (roster + tunables + refresh) and the per-account secret material.
    ///
    /// The build seam the `export` verb (issue #148) drives: this module stays a pure
    /// format layer (it never reads the keychain or the config path), so the caller
    /// gathers the pieces — `config_toml` from [`crate::config::Config::render`],
    /// `accounts` from each [`crate::stash::StashedAccount`] — and hands them in. A
    /// secret-free (`--no-secrets`) export passes an empty `accounts` vec: the roster
    /// still travels inside `config_toml`, but no credential material does.
    pub(crate) fn new(config_toml: String, accounts: Vec<ManagedAccount>) -> Self {
        Self {
            config_toml,
            accounts,
        }
    }

    /// The rendered `config.toml` (roster + tunables + refresh) the artifact carries.
    /// The read-side companion of [`new`](Payload::new)'s `config_toml` argument, the
    /// seam the `import` verb (issue #149) parses back into a [`crate::config::Config`].
    pub(crate) fn config_toml(&self) -> &str {
        &self.config_toml
    }

    /// The per-account secret material, one entry per roster `[[account]]` that carries
    /// a credential (EMPTY for a config-only / `--no-secrets` artifact). The read-side
    /// companion of [`new`](Payload::new)'s `accounts` argument.
    pub(crate) fn accounts(&self) -> &[ManagedAccount] {
        &self.accounts
    }
}

impl ManagedAccount {
    /// One account's restorable secret material, keyed by its roster `account_uuid`.
    ///
    /// `credential` is the raw `Claude Code-credentials` bearer blob
    /// ([`crate::keychain::Credential::expose`]); `oauth_account` is the canonical
    /// `oauthAccount` identity JSON ([`crate::claude_state::OauthAccount::raw_json`]).
    /// Both travel hex-encoded (see the field docs); this module stores them opaquely.
    pub(crate) fn new(account_uuid: String, credential: Vec<u8>, oauth_account: Vec<u8>) -> Self {
        Self {
            account_uuid,
            credential,
            oauth_account,
        }
    }

    /// The roster key (`account_uuid`) this stash belongs to — matches a `[[account]]`
    /// entry in [`Payload::config_toml`]. The read-side companion the `import` verb
    /// (issue #149) keys the conflict policy on. Non-secret.
    pub(crate) fn account_uuid(&self) -> &str {
        &self.account_uuid
    }

    /// The raw `Claude Code-credentials` bearer blob, as opaque bytes — restored
    /// verbatim into the target's keychain stash on import ([`crate::stash`]). SECRET:
    /// never logged; hashed, not printed, for read-back verification.
    pub(crate) fn credential(&self) -> &[u8] {
        &self.credential
    }

    /// The account's `oauthAccount` identity JSON, as opaque bytes — restored into the
    /// stash on import. SECRET (carries the account email): never logged.
    pub(crate) fn oauth_account(&self) -> &[u8] {
        &self.oauth_account
    }
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

    /// Whether the body is ciphertext (needs a passphrase to [`decrypt`](Self::decrypt))
    /// or plaintext (readable directly via [`into_plaintext_payload`](Self::into_plaintext_payload)).
    /// The `import` verb (issue #149) reads this to decide whether to acquire a passphrase
    /// at all — a plaintext artifact needs none.
    pub(crate) fn is_encrypted(&self) -> bool {
        self.header.encrypted
    }

    /// Consume a PLAINTEXT artifact and return its [`Payload`] by value.
    ///
    /// The counterpart of [`decrypt`](Self::decrypt) for the `encrypted: false` case:
    /// no key, no passphrase. [`from_bytes`](Self::from_bytes) already cross-checked the
    /// `encrypted` flag against the body, so a well-formed plaintext artifact always
    /// yields `Ok`; the `Ciphertext` arm is a defensive [`Error::MigrationInvalid`] for a
    /// caller that skipped the [`is_encrypted`](Self::is_encrypted) check. Moves (never
    /// clones) the secret-bearing payload out.
    pub(crate) fn into_plaintext_payload(self) -> Result<Payload> {
        match self.body {
            Body::Plaintext(payload) => Ok(payload),
            Body::Ciphertext(_) => Err(Error::MigrationInvalid(
                "expected a plaintext artifact — this one is encrypted",
            )),
        }
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

// --- Encryption envelope (issue #147) -------------------------------------------
//
// The optional passphrase-encryption layer over the artifact body. The container
// above defines the `encrypted` flag and the KDF / cipher parameter slots; this
// section FILLS them: derive a key from a passphrase with Argon2id, encrypt the
// serialized payload with XChaCha20-Poly1305 (an AEAD), and bind the header as
// associated data so any tamper or downgrade fails closed. No home-rolled crypto —
// only the container framing is ours; the primitives are the RustCrypto `argon2` and
// `chacha20poly1305` crates, used as documented.
//
// This is the crypto LAYER only: it adds no CLI verbs (the export / import commands
// are later work items). It provides the reusable primitives those commands consume —
// `MigrationArtifact::{encrypt, decrypt}`, passphrase acquisition, and the plaintext
// opt-out warning.

/// XChaCha20-Poly1305 key length (256-bit).
const KEY_LEN: usize = 32;
/// XChaCha20 nonce length (192-bit — the "X" extended nonce, wide enough to generate
/// at random per artifact without a birthday-bound collision worry).
const NONCE_LEN: usize = 24;
/// Argon2id salt length (128-bit), generated fresh per artifact.
const SALT_LEN: usize = 16;

/// KDF identifier written into (and required on read from) an encrypted header.
const KDF_ARGON2ID: &str = "argon2id";
/// Cipher identifier written into (and required on read from) an encrypted header.
const CIPHER_XCHACHA20POLY1305: &str = "xchacha20poly1305";

/// Argon2id cost used when WRITING a new artifact: ~64 MiB, 3 passes, single lane.
/// Recorded in the header ([`KdfParams`]) so a future cost change still reads old
/// files — a decrypt derives with the cost the *file* carries, never these. The lane
/// count is 1 deliberately: the `argon2` crate derives single-threaded unless its
/// rayon-backed `parallel` feature is enabled, which we avoid to keep the dependency
/// surface minimal — a higher lane count would only add cost without the intended
/// parallel defense.
const ARGON2_MEMORY_KIB: u32 = 65_536;
const ARGON2_ITERATIONS: u32 = 3;
const ARGON2_PARALLELISM: u32 = 1;

/// The Argon2id cost used when WRITING a new encrypted artifact — memory, time, and lane
/// cost. Recorded in the header on write ([`KdfParams`]) so a decrypt derives with the cost
/// the FILE carries, never these; a future- or operator-raised cost still decrypts.
///
/// The operator-facing source of the write-path cost is the `[migration]` config block
/// (issue #150): `export` derives an artifact's key at [`crate::config::MigrationConfig`]'s
/// cost through [`MigrationArtifact::encrypt_with_cost`]. [`PRODUCTION`](KdfCost::PRODUCTION)
/// is the built-in default that block's defaults mirror (kept in sync by a `config` test), and
/// the cost [`MigrationArtifact::encrypt`] uses when no explicit cost is supplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct KdfCost {
    /// Argon2 memory cost, in KiB.
    pub(crate) memory_kib: u32,
    /// Argon2 time cost, in iterations.
    pub(crate) iterations: u32,
    /// Argon2 parallelism, in lanes.
    pub(crate) parallelism: u32,
}

impl KdfCost {
    /// The built-in production cost: ~64 MiB, 3 passes, single lane. The default the
    /// `[migration]` config block (issue #150) mirrors, held to it by that module's
    /// `migration_kdf_defaults_match_the_crypto` test.
    pub(crate) const PRODUCTION: KdfCost = KdfCost {
        memory_kib: ARGON2_MEMORY_KIB,
        iterations: ARGON2_ITERATIONS,
        parallelism: ARGON2_PARALLELISM,
    };
}

/// The warning a `--plaintext` (unencrypted) export must print: the artifact then
/// holds usable, restorable account credentials in the clear. The export command (a
/// later work item) prints it; the wording lives here so the crypto layer owns it.
pub(crate) const PLAINTEXT_WARNING: &str =
    "WARNING: this migration artifact is UNENCRYPTED — it contains usable Claude Code \
     account credentials in the clear. Anyone who can read the file can restore your \
     accounts. Store it like a password and delete it as soon as the import is done.";

impl Header {
    /// The header, serialized, used as the AEAD **associated data** so the whole
    /// header — version, `encrypted` flag, and KDF + cipher parameters — is
    /// authenticated alongside the ciphertext: any tamper or downgrade of a header
    /// field changes these bytes and the Poly1305 tag no longer verifies, so
    /// decryption fails closed.
    ///
    /// Computed identically on encrypt (from the header just built) and decrypt (from
    /// the header just parsed). serde's struct serialization is deterministic here —
    /// fixed field order, no maps or floats, and an encrypted header always carries
    /// both parameter blocks (nothing is skipped) — so equal headers yield identical
    /// bytes on both sides.
    fn associated_data(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serializing a header cannot fail")
    }
}

impl MigrationArtifact {
    /// Encrypt `payload` under `passphrase` into a version-current ENCRYPTED artifact.
    ///
    /// Derives a key with Argon2id (fresh 128-bit salt), encrypts the serialized
    /// payload with XChaCha20-Poly1305 (fresh 192-bit nonce) binding the header as
    /// associated data, and records the KDF + cipher parameters in the header so the
    /// file is self-describing for [`decrypt`](Self::decrypt). Uses the production
    /// Argon2id cost ([`KdfCost::PRODUCTION`]); the `export` verb overrides it with the
    /// operator's `[migration]` cost via [`encrypt_with_cost`](Self::encrypt_with_cost).
    pub(crate) fn encrypt(payload: &Payload, passphrase: &Passphrase) -> Result<Self> {
        Self::encrypt_with_cost(payload, passphrase, &KdfCost::PRODUCTION)
    }

    /// [`encrypt`](Self::encrypt) at an explicit [`KdfCost`] — the seam the `export` verb
    /// (issue #150) drives so an artifact's key is derived at the operator's `[migration]`
    /// Argon2id cost. The cost is recorded in the header either way, so the artifact still
    /// round-trips through [`decrypt`](Self::decrypt) whatever cost it was written at.
    pub(crate) fn encrypt_with_cost(
        payload: &Payload,
        passphrase: &Passphrase,
        cost: &KdfCost,
    ) -> Result<Self> {
        Self::encrypt_with(
            payload,
            passphrase,
            cost.memory_kib,
            cost.iterations,
            cost.parallelism,
        )
    }

    /// [`encrypt`](Self::encrypt) with explicit scalar Argon2id cost parameters. Split out
    /// so tests can derive at a trivial cost; the parameters are recorded in the header
    /// either way, so a low-cost artifact still round-trips through
    /// [`decrypt`](Self::decrypt).
    fn encrypt_with(
        payload: &Payload,
        passphrase: &Passphrase,
        memory_kib: u32,
        iterations: u32,
        parallelism: u32,
    ) -> Result<Self> {
        // Fresh per-artifact salt and nonce from the OS CSPRNG.
        let mut salt = vec![0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);

        // Build the header FIRST (with the salt/nonce/cost it will carry) so it can be
        // bound as associated data below — the ciphertext authenticates the exact
        // header stored beside it.
        let header = Header {
            format_version: FORMAT_VERSION,
            encrypted: true,
            kdf: Some(KdfParams {
                algorithm: KDF_ARGON2ID.to_owned(),
                salt,
                memory_kib,
                iterations,
                parallelism,
            }),
            cipher: Some(CipherParams {
                algorithm: CIPHER_XCHACHA20POLY1305.to_owned(),
                nonce: nonce.to_vec(),
            }),
        };

        let key = derive_key(passphrase, header.kdf.as_ref().expect("kdf set above"))?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice())
            .expect("a derived key is exactly the cipher key length");

        // Serialize the payload (secret) and encrypt it, authenticating the header.
        let mut plaintext =
            Zeroizing::new(serde_json::to_vec(payload).expect("serializing a payload cannot fail"));
        let aad = header.associated_data();
        let ciphertext = cipher
            .encrypt(
                &nonce,
                AeadPayload {
                    msg: &plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| Error::MigrationEncryptFailed)?;
        plaintext.zeroize(); // also cleared on drop; explicit to shorten its lifetime.

        Ok(Self {
            magic: MAGIC.to_owned(),
            header,
            body: Body::Ciphertext(ciphertext),
        })
    }

    /// Decrypt an ENCRYPTED artifact under `passphrase`, returning its [`Payload`].
    ///
    /// Fails CLOSED: a wrong passphrase, a tampered / downgraded header, or a tampered
    /// / truncated body all make the AEAD tag verification fail, returning
    /// [`Error::MigrationDecryptFailed`] with ZERO plaintext produced. A non-encrypted
    /// artifact, an unknown algorithm, or malformed parameters return
    /// [`Error::MigrationCryptoParams`] before any key derivation. The decrypted buffer
    /// is held in a zeroized-on-drop wrapper.
    pub(crate) fn decrypt(&self, passphrase: &Passphrase) -> Result<Payload> {
        let (kdf, cipher_params, ciphertext) =
            match (&self.header.kdf, &self.header.cipher, &self.body) {
                (Some(kdf), Some(cipher_params), Body::Ciphertext(ciphertext)) => {
                    (kdf, cipher_params, ciphertext)
                }
                _ => {
                    return Err(Error::MigrationCryptoParams(
                        "the artifact is not encrypted",
                    ))
                }
            };
        if cipher_params.algorithm != CIPHER_XCHACHA20POLY1305 {
            return Err(Error::MigrationCryptoParams("unsupported cipher algorithm"));
        }
        if cipher_params.nonce.len() != NONCE_LEN {
            return Err(Error::MigrationCryptoParams("wrong cipher nonce length"));
        }

        let key = derive_key(passphrase, kdf)?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_slice())
            .expect("a derived key is exactly the cipher key length");
        let nonce = XNonce::from_slice(&cipher_params.nonce);
        let aad = self.header.associated_data();

        // The AEAD verifies the tag BEFORE yielding any bytes; on failure it returns
        // Err and no plaintext is produced (fail-closed). Hold the decrypted bytes in
        // a zeroized-on-drop buffer so they are wiped after deserialization.
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    nonce,
                    AeadPayload {
                        msg: ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| Error::MigrationDecryptFailed)?,
        );
        // A parse failure is redacted to a position (never bytes), mirroring
        // [`MigrationArtifact::from_bytes`].
        serde_json::from_slice::<Payload>(&plaintext).map_err(redact)
    }
}

/// Derive the XChaCha20-Poly1305 key from `passphrase` and the header's KDF
/// parameters with Argon2id. The parameters come from the artifact (not hardcoded),
/// so an old file derives with the cost it was written with. Returns a
/// zeroized-on-drop key buffer. Fails with [`Error::MigrationCryptoParams`] for an
/// unrecognized algorithm or out-of-range cost.
fn derive_key(passphrase: &Passphrase, kdf: &KdfParams) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    if kdf.algorithm != KDF_ARGON2ID {
        return Err(Error::MigrationCryptoParams(
            "unsupported key-derivation algorithm",
        ));
    }
    // The cost parameters come from a potentially UNTRUSTED artifact and are consumed
    // to derive the key BEFORE the AEAD tag can be checked — so an oversized cost would
    // hang (huge memory + CPU) or, worse, ABORT the process: `argon2`'s memory block is
    // an infallible `vec!` that panics-to-abort on OOM, never a fail-closed error, and
    // `Params::new` itself imposes no upper bound (its max is `u32::MAX`). Reject abusive
    // values up front. The ceilings sit well above the production cost
    // ([`ARGON2_MEMORY_KIB`] / [`ARGON2_ITERATIONS`] / [`ARGON2_PARALLELISM`]) so a
    // legitimate artifact — including a future one written with a raised cost — still
    // decrypts, yet far below a denial-of-service. A safety guard bounding the work, NOT
    // a format constraint (the parameters still travel in the file per #146).
    const MAX_MEMORY_KIB: u32 = 1 << 20; // 1 GiB — 16× the 64 MiB production cost.
    const MAX_ITERATIONS: u32 = 16; // ~5× the production 3 passes.
    const MAX_PARALLELISM: u32 = 8; // production is 1 (derivation is single-threaded).
    if kdf.memory_kib > MAX_MEMORY_KIB
        || kdf.iterations > MAX_ITERATIONS
        || kdf.parallelism > MAX_PARALLELISM
    {
        return Err(Error::MigrationCryptoParams(
            "Argon2 cost parameters out of range",
        ));
    }
    let params = Params::new(
        kdf.memory_kib,
        kdf.iterations,
        kdf.parallelism,
        Some(KEY_LEN),
    )
    .map_err(|_| Error::MigrationCryptoParams("invalid Argon2 parameters"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon
        .hash_password_into(passphrase.as_bytes(), &kdf.salt, key.as_mut_slice())
        .map_err(|_| Error::MigrationCryptoParams("key derivation failed"))?;
    Ok(key)
}

/// A validated, non-empty passphrase held in a zeroized-on-drop buffer.
///
/// Every constructor funnels through [`Passphrase::new`], which rejects an EMPTY
/// passphrase as a hard error ([`Error::MigrationEmptyPassphrase`]): encrypt mode must
/// never silently fall back to plaintext or "encrypt" under an empty key. The input
/// paths ([`from_file`](Self::from_file) / [`from_stdin`](Self::from_stdin) /
/// [`prompt`](Self::prompt)) exist so a passphrase is NEVER passed as a command-line
/// argument. No `Debug`: the bytes are secret.
pub(crate) struct Passphrase(Zeroizing<Vec<u8>>);

impl Passphrase {
    /// Wrap raw passphrase bytes; an EMPTY passphrase is refused.
    fn new(bytes: Zeroizing<Vec<u8>>) -> Result<Self> {
        if bytes.is_empty() {
            return Err(Error::MigrationEmptyPassphrase);
        }
        Ok(Self(bytes))
    }

    /// The passphrase bytes, for key derivation.
    fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Read a passphrase from a file's first line (a trailing newline is stripped) —
    /// the `--passphrase-file <path>` input path.
    pub(crate) fn from_file(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)?;
        Self::from_first_line(&mut std::io::BufReader::new(file))
    }

    /// Read a passphrase from standard input's first line (trailing newline stripped).
    pub(crate) fn from_stdin() -> Result<Self> {
        Self::from_first_line(&mut std::io::stdin().lock())
    }

    /// Prompt on the controlling terminal with echo disabled, and read one line.
    pub(crate) fn prompt(prompt: &str) -> Result<Self> {
        read_interactive_passphrase(prompt)
    }

    /// Shared core: read the first line, strip a trailing `\n` (and a preceding `\r`),
    /// and reject empty. Generic over the reader so the file / stdin / terminal paths
    /// share one parse-and-validate step that the tests can exercise directly.
    fn from_first_line<R: BufRead>(reader: &mut R) -> Result<Self> {
        let mut line = Zeroizing::new(Vec::new());
        reader.read_until(b'\n', &mut line)?;
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
        }
        Self::new(line)
    }
}

/// Read a passphrase from the controlling terminal with echo disabled.
///
/// Opens `/dev/tty` directly — so a redirected stdin/stdout cannot defeat the no-echo
/// prompt — clears the `ECHO` termios flag for the read, and ALWAYS restores the
/// previous terminal state (a drop guard restores even on early return or panic).
/// macOS, matching the crate.
fn read_interactive_passphrase(prompt: &str) -> Result<Passphrase> {
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let fd = tty.as_raw_fd();

    // Snapshot the terminal attributes. SAFETY: `termios` is plain-old-data that
    // `tcgetattr` fully initializes; `fd` is a live descriptor owned by `tty`.
    let mut attrs: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut attrs) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    // Clear ECHO for the read; arm the restore guard BEFORE changing the terminal.
    let mut quiet = attrs;
    quiet.c_lflag &= !libc::ECHO;
    let _restore = TermiosRestore { fd, attrs };
    // SAFETY: same live `fd`; `quiet` is a copy of the just-read attributes with one
    // flag cleared. TCSAFLUSH applies after pending output drains and discards pending
    // input, so nothing typed before the prompt leaks into the read.
    if unsafe { libc::tcsetattr(fd, libc::TCSAFLUSH, &quiet) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut out = &tty;
    write!(out, "{prompt}")?;
    out.flush()?;
    let passphrase = Passphrase::from_first_line(&mut std::io::BufReader::new(&tty));
    // The suppressed Enter left the cursor on the prompt line; advance it.
    let _ = writeln!(out);
    passphrase
}

/// Restores terminal attributes when dropped, re-enabling `ECHO` even if the
/// passphrase read errored or panicked.
struct TermiosRestore {
    fd: std::os::fd::RawFd,
    attrs: libc::termios,
}

impl Drop for TermiosRestore {
    fn drop(&mut self) {
        // SAFETY: `fd` is the same live descriptor; `attrs` is the snapshot taken
        // before the terminal was modified. Best-effort — nothing to do on failure.
        unsafe { libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.attrs) };
    }
}

/// Hex codec for the byte fields, as a serde `with` adapter, so opaque secret bytes
/// and the salt/nonce/ciphertext travel as pure-ASCII strings in the JSON container.
///
/// A thin serde shim over the shared [`crate::hex`] codec (issue #179 consolidated the
/// previously-duplicated per-module copies); this module only adapts that codec to
/// serde's `serialize_with` / `deserialize_with` shape.
mod hex_bytes {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    /// Serialize bytes as a lowercase hex string.
    pub(super) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&crate::hex::encode(bytes))
    }

    /// Deserialize a hex string back to bytes, rejecting a non-hex / odd-length
    /// value with a content-free error (only the position, added by the format,
    /// reaches the caller — see [`super::redact`]).
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<u8>, D::Error> {
        let hex = String::deserialize(deserializer)?;
        crate::hex::decode(hex.as_bytes()).ok_or_else(|| D::Error::custom("invalid hex encoding"))
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

    // --- Encryption envelope (issue #147) ------------------------------------
    //
    // Hermetic crypto tests. Argon2id at the production cost is ~64 MiB × 3, so the
    // round-trip / tamper cases derive at a TRIVIAL cost (recorded in the header, so
    // the low-cost artifact still round-trips); one dedicated test exercises the real
    // production cost and asserts the recorded parameters.

    /// Trivial Argon2id cost for fast tests (the minimum: m = 8 KiB, t = 1, p = 1).
    const TEST_M: u32 = 8;
    const TEST_T: u32 = 1;
    const TEST_P: u32 = 1;

    /// A non-empty test passphrase.
    fn test_passphrase(s: &str) -> Passphrase {
        Passphrase::new(Zeroizing::new(s.as_bytes().to_vec())).expect("non-empty passphrase")
    }

    #[test]
    fn encrypt_then_decrypt_round_trips_the_payload_and_hides_it() {
        // AC: encrypt→decrypt round-trip. Encrypt, then prove the artifact survives a
        // full on-disk round-trip (to_bytes/from_bytes, incl. `validate`) and decrypts
        // back to the ORIGINAL payload with the same passphrase.
        let payload = sample_payload();
        let pass = test_passphrase("correct horse battery staple");

        let artifact =
            MigrationArtifact::encrypt_with(&payload, &pass, TEST_M, TEST_T, TEST_P).unwrap();
        assert!(
            artifact.header.encrypted,
            "an encrypted artifact must say so"
        );

        let on_disk = artifact.to_bytes();
        let restored = MigrationArtifact::from_bytes(&on_disk).unwrap();
        let decrypted = restored.decrypt(&pass).unwrap();
        assert!(
            decrypted == payload,
            "decrypted payload must equal the original"
        );

        // The plaintext genuinely is NOT in the artifact bytes — the body is opaque
        // ciphertext, not the payload in the clear.
        let text = String::from_utf8_lossy(&on_disk);
        assert!(
            !text.contains("sk-ant-oat-EXAMPLE"),
            "a bearer token leaked"
        );
        assert!(
            !text.contains("poll_secs"),
            "the config leaked in the clear"
        );
        assert!(!text.contains("config_toml"), "a payload field name leaked");
    }

    #[test]
    fn a_wrong_passphrase_fails_authentication_with_zero_plaintext() {
        // AC: wrong passphrase → authentication fails → clear error, ZERO plaintext.
        // The `Err` arm returns no `Payload` at all.
        let artifact = MigrationArtifact::encrypt_with(
            &sample_payload(),
            &test_passphrase("right"),
            TEST_M,
            TEST_T,
            TEST_P,
        )
        .unwrap();
        let restored = MigrationArtifact::from_bytes(&artifact.to_bytes()).unwrap();
        match restored.decrypt(&test_passphrase("wrong")) {
            Err(Error::MigrationDecryptFailed) => {}
            Err(other) => panic!("expected MigrationDecryptFailed, got {other:?}"),
            Ok(_) => panic!("a wrong passphrase must never decrypt"),
        }
    }

    #[test]
    fn a_tampered_ciphertext_fails_closed() {
        // AC: a tampered file → authentication fails. Flip one nibble of the ciphertext
        // body; even the CORRECT passphrase must not decrypt it.
        let artifact = MigrationArtifact::encrypt_with(
            &sample_payload(),
            &test_passphrase("pw"),
            TEST_M,
            TEST_T,
            TEST_P,
        )
        .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&artifact.to_bytes()).unwrap();
        let ct = value["body"]["data"].as_str().unwrap().to_owned();
        let mut chars: Vec<char> = ct.chars().collect();
        chars[0] = if chars[0] == '0' { '1' } else { '0' };
        value["body"]["data"] = serde_json::Value::String(chars.into_iter().collect());
        let tampered = serde_json::to_vec(&value).unwrap();

        let restored = MigrationArtifact::from_bytes(&tampered).unwrap();
        assert!(matches!(
            restored.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationDecryptFailed)
        ));
    }

    #[test]
    fn a_downgraded_header_fails_closed() {
        // AC: a downgraded file fails closed. The header is bound as AAD, so silently
        // weakening a KDF cost parameter (iterations 2 → 1) invalidates the tag: even
        // the CORRECT passphrase cannot decrypt the altered header.
        let artifact = MigrationArtifact::encrypt_with(
            &sample_payload(),
            &test_passphrase("pw"),
            16,
            2,
            TEST_P,
        )
        .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&artifact.to_bytes()).unwrap();
        value["header"]["kdf"]["iterations"] = serde_json::json!(1);
        let tampered = serde_json::to_vec(&value).unwrap();

        let restored = MigrationArtifact::from_bytes(&tampered).unwrap();
        assert!(matches!(
            restored.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationDecryptFailed)
        ));
    }

    #[test]
    fn the_associated_data_binds_every_header_field() {
        // The mechanism behind downgrade-resistance: the AAD (authenticated by the
        // tag) changes when ANY header field changes — so the WHOLE header is bound,
        // not just coincidentally protected by key derivation.
        let base = Header {
            format_version: FORMAT_VERSION,
            encrypted: true,
            kdf: Some(KdfParams {
                algorithm: KDF_ARGON2ID.to_owned(),
                salt: vec![1, 2, 3, 4],
                memory_kib: 8,
                iterations: 1,
                parallelism: 1,
            }),
            cipher: Some(CipherParams {
                algorithm: CIPHER_XCHACHA20POLY1305.to_owned(),
                nonce: vec![0; NONCE_LEN],
            }),
        };
        let base_aad = base.associated_data();

        let mut m = base.clone();
        m.encrypted = false;
        assert_ne!(m.associated_data(), base_aad, "`encrypted` flag not bound");

        let mut m = base.clone();
        m.kdf.as_mut().unwrap().memory_kib = 16;
        assert_ne!(m.associated_data(), base_aad, "memory_kib not bound");

        let mut m = base.clone();
        m.kdf.as_mut().unwrap().iterations = 2;
        assert_ne!(m.associated_data(), base_aad, "iterations not bound");

        let mut m = base.clone();
        m.kdf.as_mut().unwrap().salt = vec![9, 9, 9, 9];
        assert_ne!(m.associated_data(), base_aad, "salt not bound");

        let mut m = base.clone();
        m.cipher.as_mut().unwrap().nonce = vec![7; NONCE_LEN];
        assert_ne!(m.associated_data(), base_aad, "nonce not bound");

        // The untouched clone reproduces the exact AAD — determinism both sides rely on.
        assert_eq!(base.clone().associated_data(), base_aad);
    }

    #[test]
    fn a_truncated_ciphertext_fails_closed() {
        // AC: a truncated file → authentication fails. Drop the trailing 32 hex chars
        // (≥ the 16-byte Poly1305 tag); the shortened body must not decrypt.
        let artifact = MigrationArtifact::encrypt_with(
            &sample_payload(),
            &test_passphrase("pw"),
            TEST_M,
            TEST_T,
            TEST_P,
        )
        .unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&artifact.to_bytes()).unwrap();
        let ct = value["body"]["data"].as_str().unwrap();
        let truncated = ct[..ct.len().saturating_sub(32)].to_owned();
        value["body"]["data"] = serde_json::Value::String(truncated);
        let tampered = serde_json::to_vec(&value).unwrap();

        let restored = MigrationArtifact::from_bytes(&tampered).unwrap();
        assert!(matches!(
            restored.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationDecryptFailed)
        ));
    }

    #[test]
    fn an_empty_passphrase_is_a_hard_error() {
        // AC: an empty passphrase in encrypt mode is a HARD error — never a silent
        // plaintext fall-back, never an empty key. The check lives in the one
        // constructor every input path funnels through.
        assert!(matches!(
            Passphrase::new(Zeroizing::new(Vec::new())),
            Err(Error::MigrationEmptyPassphrase)
        ));
        // A bare newline (an empty first line) is refused.
        assert!(matches!(
            Passphrase::from_first_line(&mut b"\n".as_slice()),
            Err(Error::MigrationEmptyPassphrase)
        ));
        // Empty input (immediate EOF) is refused.
        assert!(matches!(
            Passphrase::from_first_line(&mut b"".as_slice()),
            Err(Error::MigrationEmptyPassphrase)
        ));
    }

    #[test]
    fn the_plaintext_opt_out_is_unencrypted_with_a_prominent_warning() {
        // AC: `--plaintext` → an unencrypted artifact (encrypted:false) + a prominent
        // warning that the file holds usable credentials.
        let artifact = MigrationArtifact::plaintext(sample_payload());
        assert!(
            !artifact.header.encrypted,
            "the --plaintext path is encrypted:false"
        );
        // Decrypting a plaintext artifact is a clean typed error — no crypto attempted.
        assert!(matches!(
            artifact.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationCryptoParams(_))
        ));
        // The warning the export command prints names the risk prominently.
        assert!(
            PLAINTEXT_WARNING.contains("WARNING"),
            "warning must be prominent"
        );
        let lower = PLAINTEXT_WARNING.to_lowercase();
        assert!(
            lower.contains("unencrypted"),
            "warning must say unencrypted"
        );
        assert!(
            lower.contains("credential"),
            "warning must name credentials"
        );
    }

    #[test]
    fn passphrase_reading_takes_the_first_line_and_strips_the_newline() {
        // The shared read path: first line only, trailing LF / CRLF stripped, inner
        // spaces preserved, EOF-without-newline taken as-is. (The interactive terminal
        // path funnels through this same core; only its TTY plumbing is untested.)
        let read = |bytes: &[u8]| {
            Passphrase::from_first_line(&mut { bytes })
                .unwrap()
                .as_bytes()
                .to_vec()
        };
        assert_eq!(read(b"hunter2\n"), b"hunter2");
        assert_eq!(read(b"hunter2\r\n"), b"hunter2");
        assert_eq!(read(b"hunter2"), b"hunter2");
        assert_eq!(read(b"two words\n"), b"two words");
        assert_eq!(
            read(b"first\nignored"),
            b"first",
            "only the first line is taken"
        );
    }

    #[test]
    fn a_passphrase_file_is_read_and_an_empty_file_is_refused() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        let path = dir.path().join("pass");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"filepass\n")
            .unwrap();
        assert_eq!(
            Passphrase::from_file(&path).unwrap().as_bytes(),
            b"filepass"
        );

        let empty = dir.path().join("empty");
        std::fs::File::create(&empty).unwrap();
        assert!(matches!(
            Passphrase::from_file(&empty),
            Err(Error::MigrationEmptyPassphrase)
        ));
    }

    #[test]
    fn an_unsupported_kdf_or_cipher_algorithm_is_rejected_before_derivation() {
        // The parameters travel in the file (forward-compat), but an algorithm this
        // build does not implement is a clean decrypt-time refusal — before any key
        // derivation, and distinct from an auth failure.
        let unsupported_kdf = KdfParams {
            algorithm: "scrypt".to_owned(),
            salt: vec![0; SALT_LEN],
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        };
        let good_cipher = CipherParams {
            algorithm: CIPHER_XCHACHA20POLY1305.to_owned(),
            nonce: vec![0; NONCE_LEN],
        };
        let artifact = MigrationArtifact::encrypted(vec![0xaa; 48], unsupported_kdf, good_cipher);
        assert!(matches!(
            artifact.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationCryptoParams(_))
        ));

        let good_kdf = KdfParams {
            algorithm: KDF_ARGON2ID.to_owned(),
            salt: vec![0; SALT_LEN],
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        };
        let unsupported_cipher = CipherParams {
            algorithm: "aes-256-gcm".to_owned(),
            nonce: vec![0; NONCE_LEN],
        };
        let artifact = MigrationArtifact::encrypted(vec![0xaa; 48], good_kdf, unsupported_cipher);
        assert!(matches!(
            artifact.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationCryptoParams(_))
        ));
    }

    #[test]
    fn a_wrong_length_nonce_is_rejected_as_a_crypto_parameter_error() {
        // A 12-byte nonce (AES-GCM sized, not XChaCha20's 24) is refused as malformed
        // parameters, not surfaced as an opaque auth failure.
        let kdf = KdfParams {
            algorithm: KDF_ARGON2ID.to_owned(),
            salt: vec![0; SALT_LEN],
            memory_kib: 8,
            iterations: 1,
            parallelism: 1,
        };
        let short_nonce = CipherParams {
            algorithm: CIPHER_XCHACHA20POLY1305.to_owned(),
            nonce: vec![0; 12],
        };
        let artifact = MigrationArtifact::encrypted(vec![0xaa; 48], kdf, short_nonce);
        assert!(matches!(
            artifact.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationCryptoParams(_))
        ));
    }

    #[test]
    fn an_out_of_range_argon2_cost_is_rejected_before_derivation() {
        // A malicious / corrupt header requesting an abusive Argon2 memory cost is
        // rejected as a crypto-parameter error BEFORE any derivation — never allowed to
        // hang or OOM-abort the process on an oversized allocation. The clamp fires
        // ahead of `Params::new`, so `u32::MAX` (~4 TiB) is never actually allocated.
        let kdf = KdfParams {
            algorithm: KDF_ARGON2ID.to_owned(),
            salt: vec![0; SALT_LEN],
            memory_kib: u32::MAX,
            iterations: 1,
            parallelism: 1,
        };
        let cipher = CipherParams {
            algorithm: CIPHER_XCHACHA20POLY1305.to_owned(),
            nonce: vec![0; NONCE_LEN],
        };
        let artifact = MigrationArtifact::encrypted(vec![0xaa; 48], kdf, cipher);
        assert!(matches!(
            artifact.decrypt(&test_passphrase("pw")),
            Err(Error::MigrationCryptoParams(_))
        ));
    }

    #[test]
    fn each_encryption_uses_a_fresh_salt_and_nonce() {
        // Same payload + passphrase, but a fresh per-artifact salt and nonce → the
        // ciphertext never repeats (no deterministic-encryption leak).
        let pass = test_passphrase("pw");
        let a = MigrationArtifact::encrypt_with(&sample_payload(), &pass, TEST_M, TEST_T, TEST_P)
            .unwrap();
        let b = MigrationArtifact::encrypt_with(&sample_payload(), &pass, TEST_M, TEST_T, TEST_P)
            .unwrap();
        assert_ne!(
            a.header.kdf.as_ref().unwrap().salt,
            b.header.kdf.as_ref().unwrap().salt,
            "the salt must be per-artifact"
        );
        assert_ne!(
            a.header.cipher.as_ref().unwrap().nonce,
            b.header.cipher.as_ref().unwrap().nonce,
            "the nonce must be per-artifact"
        );
        match (&a.body, &b.body) {
            (Body::Ciphertext(x), Body::Ciphertext(y)) => {
                assert_ne!(x, y, "the ciphertext must not repeat")
            }
            _ => panic!("expected ciphertext bodies"),
        }
    }

    #[test]
    fn encrypt_records_the_production_argon2id_and_cipher_parameters() {
        // The production `encrypt` path records the chosen Argon2id cost + a 128-bit
        // salt and names XChaCha20-Poly1305 with a 192-bit nonce, and still round-trips
        // at the real cost. (The one full-cost derivation in the suite.)
        let pass = test_passphrase("pw");
        let artifact = MigrationArtifact::encrypt(&sample_payload(), &pass).unwrap();

        let kdf = artifact.header.kdf.as_ref().unwrap();
        assert_eq!(kdf.algorithm, KDF_ARGON2ID);
        assert_eq!(kdf.memory_kib, ARGON2_MEMORY_KIB);
        assert_eq!(kdf.iterations, ARGON2_ITERATIONS);
        assert_eq!(kdf.parallelism, ARGON2_PARALLELISM);
        assert_eq!(kdf.salt.len(), SALT_LEN);

        let cipher = artifact.header.cipher.as_ref().unwrap();
        assert_eq!(cipher.algorithm, CIPHER_XCHACHA20POLY1305);
        assert_eq!(cipher.nonce.len(), NONCE_LEN);

        assert!(
            artifact.decrypt(&pass).unwrap() == sample_payload(),
            "the production path must round-trip"
        );
    }

    #[test]
    fn encrypt_with_cost_records_the_supplied_cost_and_round_trips() {
        // The seam the `export` verb (issue #150) drives with the operator's `[migration]`
        // cost: the supplied `KdfCost` is what lands in the header (so a decrypt derives at
        // exactly it), and the artifact still round-trips. Uses a TRIVIAL cost so the test
        // stays fast — the point is the cost is honoured + recorded, not its magnitude.
        let pass = test_passphrase("pw");
        let cost = KdfCost {
            memory_kib: 16,
            iterations: 2,
            parallelism: 1,
        };
        let artifact =
            MigrationArtifact::encrypt_with_cost(&sample_payload(), &pass, &cost).unwrap();

        let kdf = artifact.header.kdf.as_ref().unwrap();
        assert_eq!(kdf.memory_kib, cost.memory_kib, "memory cost not recorded");
        assert_eq!(kdf.iterations, cost.iterations, "time cost not recorded");
        assert_eq!(kdf.parallelism, cost.parallelism, "lane cost not recorded");

        let restored = MigrationArtifact::from_bytes(&artifact.to_bytes()).unwrap();
        assert!(
            restored.decrypt(&pass).unwrap() == sample_payload(),
            "an artifact written at a custom cost must round-trip"
        );

        // The built-in default IS the production cost — the anchor the `[migration]`
        // defaults mirror (config side asserts the reverse direction).
        assert_eq!(KdfCost::PRODUCTION.memory_kib, ARGON2_MEMORY_KIB);
        assert_eq!(KdfCost::PRODUCTION.iterations, ARGON2_ITERATIONS);
        assert_eq!(KdfCost::PRODUCTION.parallelism, ARGON2_PARALLELISM);
    }
}
