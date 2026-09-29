//! Connect tokens, netcode.io-style.
//!
//! A login service (not in this crate) authenticates the player, then mints a
//! token for one game server and hands it to the client over TLS:
//!
//! ```text
//! ConnectToken (client-visible, 212 B as bytes):
//!   server_id:8 | expires:8 | c2s_key:32 | s2c_key:32 | private:132
//! private := nonce:12 | ChaCha20-Poly1305(token_key, nonce,
//!              ad = protocol_id ++ server_id ++ expires,
//!              user_id:8 | c2s_key:32 | s2c_key:32 | user_data:32) | tag:16
//! ```
//!
//! The client can't read or alter `private`; it sends it to the server in the
//! handshake, together with `server_id` and `expires` in the clear. The server
//! checks those two cheaply, then opens `private` with the `token_key` it shares
//! with the login service, and so learns who the player is and the connection's
//! two keys without ever having talked to the login service.
//!
//! Tokens are meant to expire within tens of seconds: they only matter until
//! the handshake completes. Nonces are random: 96 bits keep collisions
//! negligible for up to ~2^32 tokens per `token_key`, so rotate the key well
//! before that (at a million logins a day, that's millennia).

use ring::aead::{Aad, LessSafeKey, Nonce, Tag, UnboundKey, CHACHA20_POLY1305};
use ring::rand::{SecureRandom, SystemRandom};

use crate::wire::{DecodeError, Reader, Writer};

pub type Key = [u8; 32];
pub const USER_DATA_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const SEALED_BYTES: usize = 8 + 32 + 32 + USER_DATA_BYTES;
/// The encrypted part of a token, as carried in handshake packets.
pub const PRIVATE_TOKEN_BYTES: usize = NONCE_BYTES + SEALED_BYTES + TAG_BYTES;
/// `ConnectToken::to_bytes` length.
pub const CONNECT_TOKEN_BYTES: usize = 8 + 8 + 32 + 32 + PRIVATE_TOKEN_BYTES;

/// A well-known key for examples, the sim and local tests, so a server and its
/// bots agree without configuration. Anyone can mint tokens with it: never
/// deploy a server that uses it.
pub const DEV_TOKEN_KEY: Key = *b"lattice-net dev key: NOT SECRET!";

/// Fills `buf` from the OS's secure random source.
pub(crate) fn random_bytes(buf: &mut [u8]) {
    SystemRandom::new().fill(buf).expect("the OS random source failed");
}

/// A fresh random key from the OS.
pub fn generate_key() -> Key {
    let mut k = [0; 32];
    random_bytes(&mut k);
    k
}

fn aead_key(key: &Key) -> LessSafeKey {
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, key).expect("32-byte key"))
}

/// What the client holds: where the token is valid, until when, the
/// connection's keys, and the sealed part only the server can open.
#[derive(Clone, PartialEq, Eq)]
pub struct ConnectToken {
    pub server_id: u64,
    /// Unix time in seconds after which the server refuses the token.
    pub expires: u64,
    pub client_to_server_key: Key,
    pub server_to_client_key: Key,
    pub private: [u8; PRIVATE_TOKEN_BYTES],
}

impl std::fmt::Debug for ConnectToken {
    // Never print keys.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectToken")
            .field("server_id", &self.server_id)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

/// What the server learns from a valid token.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenContents {
    pub user_id: u64,
    pub client_to_server_key: Key,
    pub server_to_client_key: Key,
    pub user_data: [u8; USER_DATA_BYTES],
}

impl std::fmt::Debug for TokenContents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenContents").field("user_id", &self.user_id).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// Minted for another server.
    WrongServer,
    Expired,
    /// Not sealed with our key for these clear fields and protocol: forged,
    /// altered, or from another protocol version.
    Invalid,
}

fn associated_data(protocol_id: u64, server_id: u64, expires: u64) -> [u8; 24] {
    let mut ad = [0; 24];
    ad[..8].copy_from_slice(&protocol_id.to_le_bytes());
    ad[8..16].copy_from_slice(&server_id.to_le_bytes());
    ad[16..].copy_from_slice(&expires.to_le_bytes());
    ad
}

impl ConnectToken {
    /// Mint a token for `user_id` on server `server_id`, with fresh connection keys.
    /// Login-service side; `token_key` is shared with the game servers.
    pub fn mint(
        token_key: &Key,
        protocol_id: u64,
        server_id: u64,
        expires: u64,
        user_id: u64,
        user_data: &[u8; USER_DATA_BYTES],
    ) -> Self {
        let (c2s, s2c) = (generate_key(), generate_key());
        let mut private = [0; PRIVATE_TOKEN_BYTES];
        random_bytes(&mut private[..NONCE_BYTES]);
        let (nonce, rest) = private.split_at_mut(NONCE_BYTES);
        let (sealed, tag_out) = rest.split_at_mut(SEALED_BYTES);
        sealed[..8].copy_from_slice(&user_id.to_le_bytes());
        sealed[8..40].copy_from_slice(&c2s);
        sealed[40..72].copy_from_slice(&s2c);
        sealed[72..].copy_from_slice(user_data);
        let nonce = Nonce::try_assume_unique_for_key(nonce).expect("12-byte nonce");
        let tag = aead_key(token_key)
            .seal_in_place_separate_tag(nonce, Aad::from(associated_data(protocol_id, server_id, expires)), sealed)
            .expect("buffer is far below the AEAD's size limit");
        tag_out.copy_from_slice(tag.as_ref());
        Self { server_id, expires, client_to_server_key: c2s, server_to_client_key: s2c, private }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut w = Writer::with_capacity(CONNECT_TOKEN_BYTES);
        w.u64(self.server_id);
        w.u64(self.expires);
        w.bytes(&self.client_to_server_key);
        w.bytes(&self.server_to_client_key);
        w.bytes(&self.private);
        w.into_inner()
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, DecodeError> {
        let mut r = Reader::new(data);
        let token = Self {
            server_id: r.u64()?,
            expires: r.u64()?,
            client_to_server_key: r.take(32)?.try_into().unwrap(),
            server_to_client_key: r.take(32)?.try_into().unwrap(),
            private: r.take(PRIVATE_TOKEN_BYTES)?.try_into().unwrap(),
        };
        r.finish()?;
        Ok(token)
    }
}

/// Server side: opens the private part of tokens minted for this server.
pub struct TokenOpener {
    cipher: LessSafeKey,
    protocol_id: u64,
    server_id: u64,
}

impl TokenOpener {
    pub fn new(token_key: &Key, protocol_id: u64, server_id: u64) -> Self {
        Self { cipher: aead_key(token_key), protocol_id, server_id }
    }

    /// Checks the clear fields first (cheap), then authenticates and decrypts.
    pub fn open(
        &self,
        server_id: u64,
        expires: u64,
        private: &[u8; PRIVATE_TOKEN_BYTES],
        unix_now: u64,
    ) -> Result<TokenContents, TokenError> {
        if server_id != self.server_id {
            return Err(TokenError::WrongServer);
        }
        if unix_now >= expires {
            return Err(TokenError::Expired);
        }
        let (nonce, rest) = private.split_at(NONCE_BYTES);
        let (sealed, tag) = rest.split_at(SEALED_BYTES);
        let mut plain = [0u8; SEALED_BYTES];
        plain.copy_from_slice(sealed);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| TokenError::Invalid)?;
        let tag = Tag::try_from(tag).map_err(|_| TokenError::Invalid)?;
        let ad = Aad::from(associated_data(self.protocol_id, server_id, expires));
        self.cipher
            .open_in_place_separate_tag(nonce, ad, tag, &mut plain, 0..)
            .map_err(|_| TokenError::Invalid)?;
        Ok(TokenContents {
            user_id: u64::from_le_bytes(plain[..8].try_into().unwrap()),
            client_to_server_key: plain[8..40].try_into().unwrap(),
            server_to_client_key: plain[40..72].try_into().unwrap(),
            user_data: plain[72..].try_into().unwrap(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PID: u64 = 0x5EED;
    const SERVER: u64 = 7;
    const NOW: u64 = 1_790_000_000;

    fn mint(key: &Key) -> ConnectToken {
        ConnectToken::mint(key, PID, SERVER, NOW + 30, 42, &[9; USER_DATA_BYTES])
    }

    #[test]
    fn a_minted_token_opens_to_what_went_in() {
        let key = generate_key();
        let t = ConnectToken::from_bytes(&mint(&key).to_bytes()).unwrap();
        assert_eq!(t.to_bytes().len(), CONNECT_TOKEN_BYTES);
        let c = TokenOpener::new(&key, PID, SERVER).open(t.server_id, t.expires, &t.private, NOW).unwrap();
        assert_eq!(c.user_id, 42);
        assert_eq!(c.user_data, [9; USER_DATA_BYTES]);
        assert_eq!((c.client_to_server_key, c.server_to_client_key), (t.client_to_server_key, t.server_to_client_key));
        assert_ne!(t.client_to_server_key, t.server_to_client_key);
        assert_ne!(mint(&key).client_to_server_key, t.client_to_server_key, "fresh keys per token");
    }

    #[test]
    fn tokens_are_refused_for_the_wrong_server_time_key_protocol_or_bytes() {
        let key = generate_key();
        let t = mint(&key);
        let open = |o: &TokenOpener, server, expires, private: &[u8; PRIVATE_TOKEN_BYTES], now| {
            o.open(server, expires, private, now).map(|c| c.user_id)
        };
        let ours = TokenOpener::new(&key, PID, SERVER);
        assert_eq!(open(&ours, SERVER, t.expires, &t.private, NOW), Ok(42));
        assert_eq!(open(&ours, SERVER + 1, t.expires, &t.private, NOW), Err(TokenError::WrongServer));
        assert_eq!(open(&ours, SERVER, t.expires, &t.private, NOW + 30), Err(TokenError::Expired));
        // A client stretching its expiry, or a token minted for another server
        // relabeled as ours, breaks the tag.
        assert_eq!(open(&ours, SERVER, t.expires + 60, &t.private, NOW), Err(TokenError::Invalid));
        let theirs = ConnectToken::mint(&key, PID, SERVER + 1, NOW + 30, 42, &[0; USER_DATA_BYTES]);
        assert_eq!(open(&ours, SERVER, theirs.expires, &theirs.private, NOW), Err(TokenError::Invalid));
        // Another key or protocol version.
        let other_key = TokenOpener::new(&generate_key(), PID, SERVER);
        assert_eq!(open(&other_key, SERVER, t.expires, &t.private, NOW), Err(TokenError::Invalid));
        let other_protocol = TokenOpener::new(&key, PID + 1, SERVER);
        assert_eq!(open(&other_protocol, SERVER, t.expires, &t.private, NOW), Err(TokenError::Invalid));
        // Any flipped byte: nonce, ciphertext or tag.
        for i in [0, NONCE_BYTES, NONCE_BYTES + 50, PRIVATE_TOKEN_BYTES - 1] {
            let mut p = t.private;
            p[i] ^= 1;
            assert_eq!(open(&ours, SERVER, t.expires, &p, NOW), Err(TokenError::Invalid), "byte {i}");
        }
    }

    #[test]
    fn debug_output_never_shows_keys() {
        let t = mint(&[3; 32]);
        let s = format!("{t:?}");
        assert!(!s.contains("key") && !s.contains("private"), "{s}");
    }

    #[test]
    fn from_bytes_rejects_wrong_lengths() {
        let b = mint(&[3; 32]).to_bytes();
        assert!(ConnectToken::from_bytes(&b[..b.len() - 1]).is_err());
        let mut long = b.clone();
        long.push(0);
        assert!(ConnectToken::from_bytes(&long).is_err());
    }
}
