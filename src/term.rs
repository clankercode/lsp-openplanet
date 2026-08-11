//! Terminal styling for human CLI output (not LSP JSON-RPC).
//!
//! Colors are enabled when stdout is a TTY and `NO_COLOR` is unset, or when
//! `CLICOLOR_FORCE` / `FORCE_COLOR` is set. Plain text otherwise so pipes,
//! CI, and editor captures stay machine-friendly.

use std::io::{self, IsTerminal};
use std::sync::OnceLock;

/// Whether styled output should be emitted on stdout.
pub fn color_stdout() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| decide_color(io::stdout().is_terminal()))
}

/// Whether styled output should be emitted on stderr.
pub fn color_stderr() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| decide_color(io::stderr().is_terminal()))
}

fn decide_color(tty: bool) -> bool {
    if env_truthy("NO_COLOR") {
        return false;
    }
    if env_truthy("CLICOLOR_FORCE") || env_truthy("FORCE_COLOR") {
        return true;
    }
    // Classic CLICOLOR=0 disables when not forced.
    if matches!(
        std::env::var("CLICOLOR").ok().as_deref(),
        Some("0") | Some("false") | Some("no")
    ) {
        return false;
    }
    tty
}

fn env_truthy(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) if v.is_empty() => name == "NO_COLOR", // NO_COLOR present => off
        Ok(v) => !matches!(v.to_ascii_lowercase().as_str(), "0" | "false" | "no"),
        Err(_) => false,
    }
}

#[derive(Clone, Copy)]
pub struct Style {
    code: &'static str,
}

impl Style {
    pub const RESET: Self = Self { code: "\x1b[0m" };
    pub const BOLD: Self = Self { code: "\x1b[1m" };
    pub const DIM: Self = Self { code: "\x1b[2m" };
    pub const RED: Self = Self { code: "\x1b[31m" };
    pub const GREEN: Self = Self { code: "\x1b[32m" };
    pub const YELLOW: Self = Self { code: "\x1b[33m" };
    pub const BLUE: Self = Self { code: "\x1b[34m" };
    pub const MAGENTA: Self = Self { code: "\x1b[35m" };
    pub const CYAN: Self = Self { code: "\x1b[36m" };
    pub const BRIGHT_RED: Self = Self { code: "\x1b[91m" };
    pub const BRIGHT_GREEN: Self = Self { code: "\x1b[92m" };
    pub const BRIGHT_YELLOW: Self = Self { code: "\x1b[93m" };
    pub const BRIGHT_BLUE: Self = Self { code: "\x1b[94m" };
    pub const BRIGHT_CYAN: Self = Self { code: "\x1b[96m" };
}

/// Paint `text` if `enabled`, else return plain.
pub fn paint(enabled: bool, style: Style, text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if !enabled || text.is_empty() {
        return text.to_string();
    }
    format!("{}{}{}", style.code, text, Style::RESET.code)
}

pub fn bold(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::BOLD, text)
}

pub fn dim(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::DIM, text)
}

pub fn error(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::BRIGHT_RED, text)
}

pub fn warning(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::BRIGHT_YELLOW, text)
}

pub fn ok(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::BRIGHT_GREEN, text)
}

pub fn info(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::BRIGHT_CYAN, text)
}

pub fn path(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::CYAN, text)
}

pub fn loc(enabled: bool, text: impl AsRef<str>) -> String {
    paint(enabled, Style::BLUE, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_disabled_is_plain() {
        assert_eq!(paint(false, Style::RED, "hi"), "hi");
    }

    #[test]
    fn paint_enabled_wraps() {
        let s = paint(true, Style::RED, "hi");
        assert!(s.contains("hi"));
        assert!(s.starts_with("\x1b["));
        assert!(s.ends_with("\x1b[0m"));
    }
}
