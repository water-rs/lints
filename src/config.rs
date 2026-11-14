//! The `dylint.toml` `[waterui-lints]` table, shared by every configurable
//! lint — each pass reads it once in its constructor through `config()`.

use serde::Deserialize;

/// The `dylint.toml` `[waterui-lints]` table.
#[derive(Default, Deserialize)]
pub(crate) struct Config {
    /// Extra blocking-call def paths — `"a::b::c"` exact, `"a::b::*"` prefix.
    #[serde(default)]
    pub(crate) blocking_in_ui_context_paths: Vec<String>,
    /// A text key with more words than this must move to a short catalog key.
    #[serde(default)]
    pub(crate) long_text_key_words: Option<usize>,
}

/// The `[waterui-lints]` table of the crate being linted.
pub(crate) fn config() -> Config {
    dylint_linting::config_or_default(env!("CARGO_PKG_NAME"))
}
