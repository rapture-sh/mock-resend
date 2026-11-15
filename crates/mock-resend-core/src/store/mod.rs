//! The storage trait, and the one place a caller-supplied string becomes a path.
//!
//! Every operation has to work on both backends, so new storage behaviour is a method
//! on [`Store`] rather than a branch on the backend type.

pub mod disk;
pub mod memory;

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::{Error, Result};
use crate::model::{ApiKey, EmailId, Message};

/// The mailbox's persistence, behind a trait so the engine never names a backend.
///
/// Implementations are shared across tasks, so every method takes `&self`.
#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Stores a captured message, replacing any earlier version of the same id.
    async fn put(&self, message: Message) -> Result<()>;

    /// Returns the message with `id` under `key`, or `None`.
    ///
    /// An id belonging to another key reads as `None`: that is the isolation guarantee,
    /// and leaking across keys would make a parallel suite non-deterministic.
    async fn get(&self, key: &ApiKey, id: &EmailId) -> Result<Option<Message>>;

    /// Returns every message under `key`, oldest first.
    async fn list(&self, key: &ApiKey) -> Result<Vec<Message>>;

    /// Discards everything under `key`, or everything in the store when `key` is `None`.
    async fn reset(&self, key: Option<&ApiKey>) -> Result<()>;

    /// Returns every message that is not at a terminal event, across all keys.
    ///
    /// Crosses namespaces because the clock does: a tick has to find work under every
    /// key, and no caller sees the result — only the engine, which then reports each
    /// transition under the key that owns it. The engine decides what is due; the store
    /// only says what is still moving.
    async fn in_flight(&self) -> Result<Vec<Message>>;
}

/// The longest filename this will produce, in bytes.
///
/// Comfortably under the 255-byte limit of every filesystem that matters, with room for
/// an extension a backend appends.
const MAX_FILENAME_BYTES: usize = 200;

/// Names Windows reserves for devices, which cannot be used for a file at any extension.
const RESERVED_STEMS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Turns a caller-supplied string into a single filename inside `root`.
///
/// **This is the only function in the workspace allowed to do that.** Attachment
/// filenames, message ids and header values arrive as arbitrary strings, and clients
/// send `../`, leading slashes and drive prefixes; joining one onto a `PathBuf` writes
/// wherever the caller points. Resolving instead of joining is what makes that
/// impossible.
///
/// The result is always `root` plus exactly one component, so it cannot escape `root`
/// however hostile the input: every separator is treated as one, only the final segment
/// is kept, and anything outside `[A-Za-z0-9._-]` becomes an underscore.
///
/// # Errors
///
/// [`Error::Validation`] when nothing usable survives — an empty string, a bare `.` or
/// `..`, or a value that is only separators.
pub fn resolve_in(root: &Path, caller_supplied: &str) -> Result<PathBuf> {
    // Both separators, always: a client on Windows sends `\` and one on Linux sends `/`,
    // and a string is never a path here, so neither is ever a directory boundary.
    let last_segment = caller_supplied
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .ok_or_else(|| {
            Error::validation("Attachment filename must contain at least one usable character.")
        })?;

    let mut safe: String = last_segment
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => character,
            _ => '_',
        })
        .collect();

    // A name of only dots is `.` or `..` by another route, and a leading dot hides the
    // file from a developer listing the directory to see what was captured.
    let leading_dots = safe
        .chars()
        .take_while(|character| *character == '.')
        .count();
    if leading_dots > 0 {
        safe.replace_range(0..leading_dots, &"_".repeat(leading_dots));
    }

    // Every retained character is ASCII, so truncating by bytes cannot split one.
    safe.truncate(MAX_FILENAME_BYTES);

    if safe.is_empty() {
        return Err(Error::validation(
            "Attachment filename must contain at least one usable character.",
        ));
    }

    let stem = safe
        .split_once('.')
        .map_or(safe.as_str(), |(stem, _)| stem)
        .to_ascii_lowercase();

    if RESERVED_STEMS.contains(&stem.as_str()) {
        safe.insert(0, '_');
    }

    Ok(root.join(safe))
}

#[cfg(test)]
mod resolve_in {
    use std::path::{Path, PathBuf};

    use super::resolve_in;

    fn resolved(caller_supplied: &str) -> PathBuf {
        resolve_in(Path::new("/data/mail"), caller_supplied).expect("a usable name survives")
    }

    #[test]
    fn should_keep_an_ordinary_filename_as_it_is() {
        assert_eq!(resolved("invoice.pdf"), Path::new("/data/mail/invoice.pdf"));
    }

    #[test]
    fn should_contain_a_relative_traversal_inside_the_root() {
        assert_eq!(resolved("../../etc/passwd"), Path::new("/data/mail/passwd"));
    }

    #[test]
    fn should_contain_a_deep_traversal_inside_the_root() {
        let resolved = resolved("../../../../../../../../etc/shadow");

        assert_eq!(resolved, Path::new("/data/mail/shadow"));
    }

    #[test]
    fn should_strip_a_leading_slash() {
        assert_eq!(resolved("/etc/hosts"), Path::new("/data/mail/hosts"));
    }

    #[test]
    fn should_treat_a_backslash_as_a_separator_too() {
        assert_eq!(
            resolved("..\\..\\Windows\\System32\\drivers\\etc\\hosts"),
            Path::new("/data/mail/hosts")
        );
    }

    #[test]
    fn should_drop_a_windows_drive_prefix() {
        assert_eq!(
            resolved("C:\\Windows\\win.ini"),
            Path::new("/data/mail/win.ini")
        );
    }

    #[test]
    fn should_neutralise_a_bare_drive_relative_path() {
        // No separator at all, so the whole string is the segment and the colon goes.
        assert_eq!(resolved("C:passwd"), Path::new("/data/mail/C_passwd"));
    }

    #[test]
    fn should_replace_a_null_byte() {
        assert_eq!(
            resolved("report\0.pdf.exe"),
            Path::new("/data/mail/report_.pdf.exe")
        );
    }

    #[test]
    fn should_replace_a_newline() {
        assert_eq!(resolved("two\nlines"), Path::new("/data/mail/two_lines"));
    }

    #[test]
    fn should_replace_non_ascii_characters() {
        assert_eq!(
            resolved("facture-€.pdf"),
            Path::new("/data/mail/facture-_.pdf")
        );
    }

    #[test]
    fn should_not_produce_a_hidden_file() {
        assert_eq!(resolved(".bashrc"), Path::new("/data/mail/_bashrc"));
    }

    #[test]
    fn should_not_produce_a_name_of_only_dots() {
        assert_eq!(resolved("...."), Path::new("/data/mail/____"));
    }

    #[test]
    fn should_prefix_a_windows_reserved_device_name() {
        assert_eq!(resolved("NUL"), Path::new("/data/mail/_NUL"));
        assert_eq!(resolved("com1.txt"), Path::new("/data/mail/_com1.txt"));
    }

    #[test]
    fn should_not_prefix_a_name_that_merely_starts_like_a_reserved_one() {
        assert_eq!(resolved("console.log"), Path::new("/data/mail/console.log"));
    }

    #[test]
    fn should_truncate_an_overlong_name() {
        let resolved = resolved(&"a".repeat(5_000));
        let name = resolved
            .file_name()
            .and_then(|name| name.to_str())
            .expect("an ascii name");

        assert_eq!(name.len(), super::MAX_FILENAME_BYTES);
    }

    #[test]
    fn should_always_resolve_to_a_direct_child_of_the_root() {
        let root = Path::new("/data/mail");
        let hostile = [
            "../x",
            "..\\x",
            "/x",
            "////x",
            "a/../../x",
            "C:/x",
            "\\\\server\\share\\x",
            "x/..",
            "./x",
        ];

        for caller_supplied in hostile {
            let resolved =
                resolve_in(root, caller_supplied).expect("a usable name survives each of these");

            assert_eq!(
                resolved.parent(),
                Some(root),
                "{caller_supplied} escaped the root"
            );
            assert_eq!(
                resolved.components().count(),
                root.components().count() + 1,
                "{caller_supplied} produced more than one component"
            );
        }
    }

    #[test]
    fn should_reject_a_string_with_nothing_usable_in_it() {
        for caller_supplied in ["", ".", "..", "/", "///", "\\", "./", "../"] {
            assert!(
                resolve_in(Path::new("/data/mail"), caller_supplied).is_err(),
                "{caller_supplied:?} should not resolve to anything"
            );
        }
    }
}
