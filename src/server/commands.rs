//! The `workspace/executeCommand` surface (B039).
//!
//! One list, one parse, one place. `ExecuteCommandOptions.commands` is a
//! promise to every client — VS Code puts those names in front of users —
//! and B039 was filed because a name sat in that list with no client route.
//! The inverse failure is worse and invisible: a name in the list that the
//! handler does not dispatch looks, from the client's side, exactly like a
//! command that ran and did nothing.
//!
//! This module makes both mistakes hard to make:
//!
//! * [`ADVERTISED`] is what `initialize` publishes, so a command cannot be
//!   advertised without appearing here.
//! * [`ServerCommand::parse`] is what the handler dispatches on, and the
//!   handler matches the enum EXHAUSTIVELY — adding a variant without
//!   handling it does not compile.
//! * [`advertised_commands_all_parse`] closes the last gap: a name added to
//!   [`ADVERTISED`] with no matching variant goes red.
//!
//! The end-to-end proof lives in `tests/execute_command_tests.rs`, which
//! reads the advertised list back off a real `initialize` response and
//! executes every entry through the router. This module cannot prove
//! reachability on its own — that was the whole shape of the B039/B040 bug.

use tower_lsp::jsonrpc::{Error as RpcError, ErrorCode};

/// Every command name `initialize` advertises, in the order it advertises
/// them. The ONLY source for `ExecuteCommandOptions.commands`.
pub const ADVERTISED: &[&str] = &[ServerCommand::REGENERATE_TYPE_DB];

/// A command the server implements.
///
/// Deliberately an enum rather than a string match in the handler: the
/// handler's `match` is exhaustive, so a variant added here without a
/// dispatch arm is a compile error rather than a silent fall-through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerCommand {
    /// Re-read the configured type database from disk and swap it in.
    RegenerateTypeDb,
}

impl ServerCommand {
    pub const REGENERATE_TYPE_DB: &'static str = "openplanet.regenerateTypeDb";

    /// Every variant. Rust cannot enumerate an enum, so this is hand-kept —
    /// but not unguarded: `every_command_is_advertised` walks it and
    /// `the_variant_list_and_the_advertised_list_are_the_same_size` fails if
    /// a command is added to [`ADVERTISED`] and forgotten here.
    pub const ALL: &'static [ServerCommand] = &[Self::RegenerateTypeDb];

    /// The wire name. Paired with [`parse`](Self::parse) below so the two
    /// directions cannot drift.
    pub const fn name(self) -> &'static str {
        match self {
            Self::RegenerateTypeDb => Self::REGENERATE_TYPE_DB,
        }
    }

    /// Resolve a client-supplied command name. `None` is an unknown command,
    /// which the handler turns into [`unknown_command_error`] — never a
    /// silent success.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            Self::REGENERATE_TYPE_DB => Some(Self::RegenerateTypeDb),
            _ => None,
        }
    }
}

/// The list `initialize` publishes.
pub fn advertised() -> Vec<String> {
    ADVERTISED.iter().map(|name| (*name).to_string()).collect()
}

/// A command the server does not implement.
///
/// `InvalidParams`, not `MethodNotFound`: `workspace/executeCommand` exists
/// — it is the `command` parameter that names nothing. Answering `Ok(null)`
/// instead (the pre-B039 behaviour) is indistinguishable to a client from a
/// command that ran successfully and had nothing to report, which is how an
/// unreachable command can stay unreachable for a whole release.
pub fn unknown_command_error(command: &str) -> RpcError {
    RpcError {
        code: ErrorCode::InvalidParams,
        message: format!(
            "workspace/executeCommand: unknown command `{command}`; this server advertises {}",
            ADVERTISED.join(", ")
        )
        .into(),
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The half the exhaustive `match` in the handler cannot cover: a name
    /// advertised with no variant behind it.
    #[test]
    fn advertised_commands_all_parse() {
        assert!(!ADVERTISED.is_empty(), "an empty list makes this vacuous");
        for name in ADVERTISED {
            assert!(
                ServerCommand::parse(name).is_some(),
                "`{name}` is advertised but `ServerCommand::parse` does not know it"
            );
        }
    }

    /// And the inverse: an implemented command nothing tells clients about
    /// is just as unreachable as B039's was.
    #[test]
    fn every_command_is_advertised() {
        for command in ServerCommand::ALL {
            assert!(
                ADVERTISED.contains(&command.name()),
                "{command:?} is implemented but never advertised"
            );
        }
    }

    /// Closes the drift `ALL` would otherwise allow. With this plus
    /// [`advertised_commands_all_parse`] and [`every_command_is_advertised`],
    /// the two lists are the same set.
    ///
    /// Not airtight, and deliberately not claimed to be: a variant added to
    /// the enum and to `parse` but to NEITHER list is invisible to all three,
    /// because Rust cannot enumerate an enum's variants. That case is an
    /// unadvertised command — unreachable, which is the failure B039 was
    /// filed for, so it is caught by review, not by this file.
    #[test]
    fn the_variant_list_and_the_advertised_list_are_the_same_size() {
        assert_eq!(ServerCommand::ALL.len(), ADVERTISED.len());
    }

    #[test]
    fn an_unknown_name_does_not_parse() {
        assert_eq!(ServerCommand::parse("openplanet.nope"), None);
        assert_eq!(ServerCommand::parse(""), None);
        // Near-misses: case and namespace both matter.
        assert_eq!(ServerCommand::parse("openplanet.regeneratetypedb"), None);
        assert_eq!(ServerCommand::parse("regenerateTypeDb"), None);
    }

    #[test]
    fn the_unknown_command_error_names_the_command_and_the_alternatives() {
        let error = unknown_command_error("openplanet.nope");
        assert_eq!(error.code, ErrorCode::InvalidParams);
        assert!(error.message.contains("openplanet.nope"));
        assert!(error.message.contains(ServerCommand::REGENERATE_TYPE_DB));
    }
}
