#!/bin/bash
# Throwaway workspace the demo records in — keeps real paths, usernames,
# and chat history out of the shots.
set -euo pipefail
rm -rf /tmp/vibin-demo
mkdir -p /tmp/vibin-demo/src
cd /tmp/vibin-demo
cat > Cargo.toml <<'TOML'
[package]
name = "tidepool"
version = "0.3.1"
edition = "2021"
TOML
cat > src/remote.rs <<'RS'
//! Remote target stub.
pub struct Remote { pub url: String }
RS
printf '# tidepool\n\nwatch a directory, mirror changes.\n' > README.md
cat > src/main.rs <<'RS'
//! tidepool: watch a directory and mirror changes to a remote.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq)]
enum Change {
    Created(PathBuf),
    Modified(PathBuf),
    Removed(PathBuf),
}

/// Pending changes, deduplicated per path within the debounce window.
struct Batcher {
    pending: HashMap<PathBuf, Change>,
    deadline: Option<Instant>,
}

impl Batcher {
    fn new() -> Self {
        Self { pending: HashMap::new(), deadline: None }
    }

    /// Record a change; later events for the same path win.
    fn push(&mut self, change: Change) {
        let path = match &change {
            Change::Created(p) | Change::Modified(p) | Change::Removed(p) => p.clone(),
        };
        self.pending.insert(path, change);
        self.deadline.get_or_insert_with(|| Instant::now() + DEBOUNCE);
    }

    /// Take the batch if the debounce window has closed.
    fn drain(&mut self) -> Option<Vec<Change>> {
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            self.deadline = None;
            Some(self.pending.drain().map(|(_, c)| c).collect())
        } else {
            None
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let mut batcher = Batcher::new();
    let readme = Change::Created(PathBuf::from("README.md"));
    batcher.puush(readme);
    loop {
        if let Some(batch) = batcher.drain() {
            for change in &batch {
                println!("sync: {change:?}");
            }
            break;
        }
    }
}
RS
git init -qb main && git add -A && git commit -qm "init"
printf 'pub struct Remote { pub url: String, pub token: Option<u64> }\n' >> src/remote.rs
