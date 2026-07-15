//! vibin as a library: the same modules the binary uses, exposed so that
//! benches (benches/) and integration tests can drive editor features —
//! highlighting, spell-checking, rendering — without a PTY.

pub mod acp;
pub mod app;
pub mod backend;
pub mod clipboard;
pub mod color;
pub mod config;
pub mod confusable;
pub mod devicons;
pub mod diff;
pub mod editor;
pub mod filetree;
pub mod git;
pub mod hex;
pub mod imageview;
pub mod input;
pub mod keybind;
pub mod kittyanim;
pub mod lsp;
pub mod markdown;
pub mod openrouter;
pub mod palette;
pub mod parrot;
pub mod pattern;
pub mod spell;
pub mod textinput;
pub mod ui;
pub mod update;
