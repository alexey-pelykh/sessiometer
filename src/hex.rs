// Copyright (c) 2026 Oleksii PELYKH
// SPDX-License-Identifier: MIT

//! Lowercase hex codec, hand-rolled.
//!
//! Hand-rolled to keep the dependency graph minimal — the crate hand-rolls its
//! other primitives ([`crate::sha256`], the civil-date math) for the same
//! reason, and a two-digit-per-byte codec is the wrong thing to pull a runtime
//! dependency in for. This is the single internal home for the scheme that was
//! previously scattered across [`crate::stash`], [`crate::migration`],
//! [`crate::keychain`], and [`crate::sha256`].
//!
//! Encoding is required (not cosmetic) where a secret must stay pure-ASCII so
//! `find-generic-password -w` renders it as text rather than as its own `0x`-hex
//! blob — see [`crate::stash`] for the keychain round-trip that depends on it.
//! [`decode`] accepts either case and rejects a corrupted (odd-length or non-hex)
//! value.

/// Encode bytes as lowercase, two-digits-per-byte hex (always pure ASCII).
pub(crate) fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        // `from_digit(0..16, 16)` is infallible and yields `0-9a-f`.
        out.push(char::from_digit((b >> 4) as u32, 16).expect("high nibble < 16"));
        out.push(char::from_digit((b & 0x0f) as u32, 16).expect("low nibble < 16"));
    }
    out
}

/// Decode lowercase (or uppercase) hex, returning `None` for an odd length or a
/// non-hex byte — i.e. a corrupted value. An empty input decodes to empty bytes.
pub(crate) fn decode(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for pair in hex.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_renders_lowercase_two_digits_per_byte() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(&[0x00, 0xff, 0x4a]), "00ff4a");
        // ASCII text: the byte pattern the keychain `0x`-blob path decodes.
        assert_eq!(encode(b"alice"), "616c696365");
    }

    #[test]
    fn encode_then_decode_round_trips_all_byte_values_empty_and_non_ascii() {
        // Every byte value, plus a non-ASCII UTF-8 JSON string like a real
        // displayName/organizationName — the case that broke a raw round-trip and
        // is the reason a byte-exact codec is needed.
        let all_bytes: Vec<u8> = (0u8..=255).collect();
        for sample in [
            b"".as_slice(),
            b"{\"a\":1}",
            "{\"displayName\":\"Cafe\u{301} \u{41e}\u{43b}\u{435}\u{43a}\u{441}i\u{439}\"}"
                .as_bytes(),
            &all_bytes,
        ] {
            let encoded = encode(sample);
            assert!(encoded.is_ascii(), "hex output must be pure ASCII");
            assert_eq!(decode(encoded.as_bytes()).as_deref(), Some(sample));
        }
    }

    #[test]
    fn decode_rejects_odd_length_and_non_hex_and_accepts_either_case() {
        assert_eq!(decode(b"abc"), None); // odd length
        assert_eq!(decode(b"zz"), None); // non-hex digit
        assert_eq!(decode(b"6X"), None); // one bad digit

        // Uppercase is accepted (decode is case-insensitive).
        assert_eq!(decode(b"4A").as_deref(), Some(b"\x4a".as_slice()));
    }
}
