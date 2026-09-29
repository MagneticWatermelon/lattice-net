//! Minimal `--key value` / `--flag` argument parsing for the binaries.

use std::collections::HashMap;
use std::str::FromStr;

pub struct Args {
    usage: &'static str,
    values: HashMap<String, Option<String>>,
}

impl Args {
    /// Parses `std::env::args()`. Prints `usage` and exits on `--help` or junk.
    pub fn parse(usage: &'static str) -> Self {
        let mut values = HashMap::new();
        let mut it = std::env::args().skip(1).peekable();
        while let Some(a) = it.next() {
            let Some(key) = a.strip_prefix("--") else { die(usage, &format!("unexpected argument {a:?}")) };
            if key == "help" {
                println!("{usage}");
                std::process::exit(0);
            }
            let value = it.next_if(|v| !v.starts_with("--"));
            values.insert(key.to_string(), value);
        }
        Self { usage, values }
    }

    pub fn get<T: FromStr>(&mut self, key: &str, default: T) -> T
    where
        T::Err: std::fmt::Display,
    {
        self.opt(key).unwrap_or(default)
    }

    pub fn opt<T: FromStr>(&mut self, key: &str) -> Option<T>
    where
        T::Err: std::fmt::Display,
    {
        match self.values.remove(key)? {
            Some(v) => Some(v.parse().unwrap_or_else(|e| die(self.usage, &format!("--{key} {v:?}: {e}")))),
            None => die(self.usage, &format!("--{key} needs a value")),
        }
    }

    pub fn flag(&mut self, key: &str) -> bool {
        match self.values.remove(key) {
            Some(None) => true,
            Some(Some(v)) => die(self.usage, &format!("--{key} takes no value, got {v:?}")),
            None => false,
        }
    }

    /// Call after reading every option: rejects unknown ones.
    pub fn finish(self) {
        if let Some(k) = self.values.keys().next() {
            die(self.usage, &format!("unknown option --{k}"));
        }
    }
}

fn die(usage: &str, msg: &str) -> ! {
    eprintln!("error: {msg}\n\n{usage}");
    std::process::exit(2);
}
