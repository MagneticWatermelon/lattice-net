//! lattice-invite: playtest invites, standing in for a login service. Each
//! player gets a file with the server's address, a user id and a batch of
//! connect tokens minted under the server's token key
//! (`lattice_client_core::invite`); `lattice-client --invite FILE` plays with
//! it. `DIR/users.txt` keeps who got which user id, so the server's session
//! log (`lattice-server --session-log`) can be read by name, and a player
//! invited again keeps theirs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use lattice_client_core::invite::Invite;
use lattice_net::token::USER_DATA_BYTES;
use lattice_net::{Config, ConnectToken};
use lattice_sim::cli::{Args, HexKey};

const USAGE: &str = "\
lattice-invite: playtest invites (connect tokens) for a lattice-server

  lattice-invite --token-key KEY --server HOST:PORT [options] NAME...

  --token-key KEY      the server's --token-key: 64 hex digits, or @FILE to read them
  --server HOST:PORT   where players reach the server
  --server-id N        the server's --server-id [1]
  --days D             how long the tokens last [14]
  --tokens N           tokens per invite: each connects once, so one per launch of the
                       game [100]
  --out DIR            writes DIR/NAME.txt per player, and DIR/users.txt (user id and
                       name per line; a name already there keeps its id) [invites]
  NAME...              the players: letters, digits, '-', '_'";

fn main() -> std::io::Result<()> {
    let mut a = Args::parse(USAGE);
    let key: HexKey = a.get("token-key", HexKey::default());
    let server: String = a.opt("server").unwrap_or_else(|| die("--server is needed: where players reach it, HOST:PORT"));
    let server_id: u64 = a.get("server-id", 1);
    let days: f64 = a.get("days", 14.0);
    let count: usize = a.get("tokens", 100);
    let out = PathBuf::from(a.get("out", "invites".to_string()));
    let names = a.rest();
    a.finish();
    if key.is_dev() {
        die("--token-key is the public dev key: anyone can mint tokens with it, so a server reachable by players must not take it");
    }
    if names.is_empty() {
        die("name at least one player");
    }
    if let Some(bad) = names.iter().find(|n| n.is_empty() || !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')) {
        die(&format!("{bad:?}: names are letters, digits, '-' and '_'"));
    }
    std::fs::create_dir_all(&out)?;

    // Who already has a user id.
    let users_path = out.join("users.txt");
    let mut users: BTreeMap<String, u64> = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(&users_path) {
        for line in text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#')) {
            let (id, name) = line.split_once(' ').unwrap_or_else(|| die(&format!("{}: bad line {line:?}", users_path.display())));
            users.insert(name.trim().to_string(), id.parse().unwrap_or_else(|_| die(&format!("{}: bad id in {line:?}", users_path.display()))));
        }
    }

    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after 1970").as_secs();
    let expires = unix + (days * 86_400.0) as u64;
    let protocol = Config::default().protocol_id;
    for name in &names {
        let next = users.values().max().map_or(1, |m| m + 1);
        let user = *users.entry(name.clone()).or_insert(next);
        let tokens = (0..count).map(|_| ConnectToken::mint(&key.0, protocol, server_id, expires, user, &[0; USER_DATA_BYTES])).collect();
        let invite = Invite { server: server.clone(), user, name: name.clone(), tokens };
        let path = out.join(format!("{name}.txt"));
        std::fs::write(&path, invite.to_text())?;
        // A fresh invite starts its count again.
        let _ = std::fs::remove_file(path.with_extension("used"));
        println!("{}: user {user}, {count} tokens until {} days from now", path.display(), days);
    }
    let mut text = String::from("# user id and name, one per line (lattice-invite)\n");
    let mut by_id: Vec<(&u64, &String)> = users.iter().map(|(n, id)| (id, n)).collect();
    by_id.sort();
    for (id, name) in by_id {
        text.push_str(&format!("{id} {name}\n"));
    }
    std::fs::write(&users_path, text)?;
    Ok(())
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}\n\n{USAGE}");
    std::process::exit(2);
}
