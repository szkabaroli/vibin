//! vibin as a library: the same modules the binary uses, exposed so that
//! benches (benches/) and integration tests can drive editor features —
//! highlighting, spell-checking, rendering — without a PTY.

pub mod app;
pub mod chats;
pub mod clipboard;
pub mod color;
pub mod config;
pub mod confusable;
pub mod diff;
pub mod editor;
pub mod filetree;
pub mod git;
pub mod hex;
pub mod input;
pub mod lsp;
pub mod markdown;
pub mod palette;
pub mod parrot;
pub mod pattern;
pub mod projects;
pub mod session;
pub mod spell;
pub mod ui;
