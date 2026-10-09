//! Playtest invites: what a player needs to join a server without a login
//! service. A text file with where to connect, who the player is, and a
//! batch of connect tokens for them.
//!
//! ```text
//! # lattice playtest invite for alice
//! server 203.0.113.7:40000
//! user 3
//! name alice
//! token 0123…    (212 bytes as hex: `ConnectToken::to_bytes`)
//! token …
//! ```
//!
//! Each token connects once (the server remembers the ones it has seen), so
//! an invite carries many: the game takes the next one each launch and
//! counts the ones used in a file beside the invite (`Invite::next_token`). Tokens
//! hold the connection's keys: whoever has the file can play as that player
//! until the tokens run out or expire, so it's sent to them privately. A
//! server operator revokes every invite at once by changing the token key.

use std::path::Path;

use lattice_net::ConnectToken;

#[derive(Debug, Clone, PartialEq)]
pub struct Invite {
    /// Where to connect, `host:port`, as the player reaches it.
    pub server: String,
    /// The user id the tokens carry (what the server's session log keys on).
    pub user: u64,
    pub name: String,
    pub tokens: Vec<ConnectToken>,
}

impl Invite {
    pub fn parse(text: &str) -> Result<Invite, String> {
        let (mut server, mut user, mut name, mut tokens) = (None, None, String::new(), Vec::new());
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once(char::is_whitespace).map(|(k, v)| (k, v.trim())).unwrap_or((line, ""));
            let bad = |what: &str| format!("line {}: {what}", n + 1);
            match key {
                "server" => server = Some(value.to_string()),
                "user" => user = Some(value.parse().map_err(|_| bad("user: not a number"))?),
                "name" => name = value.to_string(),
                "token" => {
                    let bytes = hex(value).ok_or_else(|| bad("token: not hex"))?;
                    tokens.push(ConnectToken::from_bytes(&bytes).map_err(|_| bad("token: wrong length"))?);
                }
                _ => return Err(bad(&format!("unknown key {key:?}"))),
            }
        }
        Ok(Invite {
            server: server.ok_or("no server line")?,
            user: user.ok_or("no user line")?,
            name,
            tokens,
        })
    }

    pub fn to_text(&self) -> String {
        let mut out = format!(
            "# lattice playtest invite for {}\n# Whoever has this file can play as {0}: keep it to yourself.\nserver {}\nuser {}\nname {0}\n",
            self.name, self.server, self.user
        );
        for t in &self.tokens {
            out.push_str("token ");
            for b in t.to_bytes() {
                out.push_str(&format!("{b:02x}"));
            }
            out.push('\n');
        }
        out
    }

    /// The next token to use, by the count kept beside the invite
    /// (`invite.used` for `invite.txt`), which it moves past that token
    /// before returning it (a launch that fails
    /// after reading it has spent it anyway: the server may have seen it).
    /// Tokens expired by `unix_now` are skipped. `None` when none are left.
    pub fn next_token(&self, path: &Path, unix_now: u64) -> std::io::Result<Option<(usize, ConnectToken)>> {
        let used_path = path.with_extension("used");
        let used: usize = match std::fs::read_to_string(&used_path) {
            Ok(s) => s.trim().parse().unwrap_or(0),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e),
        };
        let Some((i, token)) = self.tokens.iter().enumerate().skip(used).find(|(_, t)| t.expires > unix_now) else {
            return Ok(None);
        };
        std::fs::write(&used_path, format!("{}\n", i + 1))?;
        Ok(Some((i, token.clone())))
    }
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_net::token::{DEV_TOKEN_KEY, USER_DATA_BYTES};

    #[test]
    fn an_invite_round_trips_and_hands_out_each_token_once() {
        let token = |expires| ConnectToken::mint(&DEV_TOKEN_KEY, 7, 1, expires, 3, &[0; USER_DATA_BYTES]);
        let invite = Invite { server: "203.0.113.7:40000".into(), user: 3, name: "alice".into(), tokens: vec![token(100), token(2000), token(3000)] };
        let back = Invite::parse(&invite.to_text()).unwrap();
        assert_eq!(back, invite);
        assert!(Invite::parse("server x\nuser 1\ntoken 00ff\n").unwrap_err().contains("wrong length"));
        assert!(Invite::parse("user 1\n").is_err());

        let dir = std::env::temp_dir().join(format!("lattice-invite-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("invite.txt");
        std::fs::write(&path, invite.to_text()).unwrap();
        // The first token has expired by 1000: skipped.
        let (i, t) = invite.next_token(&path, 1000).unwrap().unwrap();
        assert_eq!((i, t.expires), (1, 2000));
        assert_eq!(invite.next_token(&path, 1000).unwrap().unwrap().0, 2);
        assert_eq!(invite.next_token(&path, 1000).unwrap(), None, "each once");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
