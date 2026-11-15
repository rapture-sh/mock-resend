//! The disk backend, writing captured mail under `--data`.
//!
//! One `.eml` per message, which is the record rather than a side artifact: a developer
//! can open the file in a mail client and see exactly what their code produced, and the
//! state survives a container restart because the volume does.
//!
//! The directory layout is `<data>/<key>/<id>.eml`, where `<key>` is the API key made
//! safe. An API key is a caller-supplied string, so it cannot become a directory name by
//! concatenation: `re_../../etc` would walk straight out of the data directory. It goes
//! through [`resolve_in`] like every other caller-supplied string, with a digest
//! appended, because sanitising alone would let two different keys land in one
//! directory and quietly share a mailbox.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use tokio::fs;

use crate::error::{Error, Result};
use crate::mime;
use crate::model::{ApiKey, EmailId, Message};
use crate::store::{Store, resolve_in};

/// The extension every captured message is written with.
const EXTENSION: &str = "eml";

/// A [`Store`] writing `.eml` files under a data directory.
#[derive(Debug)]
pub struct DiskStore {
    root: PathBuf,
    /// Insertion order per key, so `list` is stable.
    ///
    /// Directory listings have no order worth relying on, and the file's own timestamp
    /// is the wall clock rather than the engine's, which a test can move. This is an
    /// index, not a cache: anything missing from it is read from disk.
    order: RwLock<HashMap<ApiKey, Vec<EmailId>>>,
}

/// Turns a poisoned lock into a storage failure.
fn poisoned() -> Error {
    Error::Store("the disk store's index lock was poisoned by an earlier panic".to_owned())
}

/// Turns an I/O failure into a storage failure.
fn io_failure(action: &str, error: &std::io::Error) -> Error {
    // The path is deliberately absent: it contains the API key, which is frequently a
    // test name, and a log line is not the place for it.
    Error::Store(format!("could not {action} a captured message: {error}"))
}

impl DiskStore {
    /// Opens a store at `root`, creating the directory if it is not there.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] if the directory cannot be created or read.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();

        fs::create_dir_all(&root)
            .await
            .map_err(|error| io_failure("create the data directory for", &error))?;

        let store = Self {
            root,
            order: RwLock::new(HashMap::new()),
        };

        store.rebuild_index().await?;

        Ok(store)
    }

    /// The directory this store writes to.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory a key's mail lives in.
    ///
    /// The name is the key made safe plus a digest of the whole key. The digest is what
    /// makes it correct rather than merely safe: `re_a/b` and `re_a_b` sanitise to the
    /// same thing, and two keys sharing a mailbox would break the isolation the whole
    /// project rests on.
    fn directory_for(&self, key: &ApiKey) -> Result<PathBuf> {
        let mut hasher = DefaultHasher::new();
        key.as_str().hash(&mut hasher);
        let digest = hasher.finish();

        resolve_in(&self.root, &format!("{}-{digest:016x}", key.as_str()))
    }

    /// The file one message lives in.
    fn path_for(&self, key: &ApiKey, id: &EmailId) -> Result<PathBuf> {
        let directory = self.directory_for(key)?;

        // The id is a UUID rather than a caller-supplied string, so it cannot contain a
        // separator. It still goes through the resolver, because the rule is that *no*
        // string becomes a path by concatenation, not that no risky one does.
        resolve_in(&directory, &format!("{id}.{EXTENSION}"))
    }

    /// Reads every `.eml` under `root` to restore the insertion order.
    ///
    /// Runs once, at start-up, so a restart finds the mail that was already captured.
    /// Order within a key is by capture time, which is the engine's clock as recorded in
    /// the file rather than the filesystem's idea of it.
    async fn rebuild_index(&self) -> Result<()> {
        let mut restored: HashMap<ApiKey, Vec<Message>> = HashMap::new();

        let mut directories = fs::read_dir(&self.root)
            .await
            .map_err(|error| io_failure("list the data directory of", &error))?;

        while let Some(directory) = directories
            .next_entry()
            .await
            .map_err(|error| io_failure("list the data directory of", &error))?
        {
            if !directory
                .file_type()
                .await
                .map_err(|error| io_failure("inspect", &error))?
                .is_dir()
            {
                continue;
            }

            let mut files = fs::read_dir(directory.path())
                .await
                .map_err(|error| io_failure("list a mailbox of", &error))?;

            while let Some(file) = files
                .next_entry()
                .await
                .map_err(|error| io_failure("list a mailbox of", &error))?
            {
                let path = file.path();

                if path.extension().and_then(|extension| extension.to_str()) != Some(EXTENSION) {
                    continue;
                }

                let raw = fs::read(&path)
                    .await
                    .map_err(|error| io_failure("read", &error))?;

                // A file this server did not write, or wrote in an older format, is
                // skipped rather than fatal: a developer poking around in the volume
                // should not be able to stop the server from starting.
                match mime::from_eml(&raw) {
                    Ok(message) => {
                        // Indexed under the mailbox it was found in, not under whatever
                        // the file claims. A file whose recorded owner disagrees with
                        // its location would otherwise put one key's mail into
                        // another's index on the next restart.
                        let expected = self.directory_for(&message.key)?;

                        if Some(expected.as_path()) != path.parent() {
                            tracing::warn!(
                                "skipping a file in the data directory: its recorded owner does not match the mailbox it is in"
                            );

                            continue;
                        }

                        restored
                            .entry(message.key.clone())
                            .or_default()
                            .push(message);
                    }
                    // A file this server did not write, or wrote in an older format, is
                    // skipped rather than fatal. The reason is logged; the path is not,
                    // because it contains the API key, and neither are the contents.
                    Err(error) => tracing::warn!("skipping a file in the data directory: {error}"),
                }
            }
        }

        let mut order = self.order.write().map_err(|_| poisoned())?;

        for (key, mut messages) in restored {
            messages.sort_by_key(|message| (message.created_at, message.id));
            order.insert(
                key,
                messages.into_iter().map(|message| message.id).collect(),
            );
        }

        Ok(())
    }

    /// Reads one message from disk.
    async fn read(&self, key: &ApiKey, id: &EmailId) -> Result<Option<Message>> {
        let path = self.path_for(key, id)?;

        let raw = match fs::read(&path).await {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_failure("read", &error)),
        };

        let message = mime::from_eml(&raw)?;

        // The file says who owns it, and that has to agree with the mailbox it was
        // found in. Disagreement means the file was not written by this server for this
        // key — a hand-edited `.eml`, a volume shared between two data directories, or
        // a bug — and the only safe answer is that this key has no such message.
        // Returning it would hand one key's mail to another, which is the one thing
        // per-key isolation has to prevent, and `put`ting it back would move the file.
        if message.key != *key || message.id != *id {
            tracing::warn!(
                "ignoring a file in the data directory whose recorded owner or id does not match its location"
            );

            return Ok(None);
        }

        Ok(Some(message))
    }
}

#[async_trait]
impl Store for DiskStore {
    async fn put(&self, message: Message) -> Result<()> {
        let path = self.path_for(&message.key, &message.id)?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|error| io_failure("create a mailbox for", &error))?;
        }

        let eml = mime::to_eml(&message)?;

        // Written whole rather than appended to, so a message that is updated — an
        // event transition — replaces its file instead of growing it.
        fs::write(&path, &eml)
            .await
            .map_err(|error| io_failure("write", &error))?;

        let mut order = self.order.write().map_err(|_| poisoned())?;
        let ids = order.entry(message.key.clone()).or_default();

        if !ids.contains(&message.id) {
            ids.push(message.id);
        }

        Ok(())
    }

    async fn get(&self, key: &ApiKey, id: &EmailId) -> Result<Option<Message>> {
        self.read(key, id).await
    }

    async fn list(&self, key: &ApiKey) -> Result<Vec<Message>> {
        let ids = {
            let order = self.order.read().map_err(|_| poisoned())?;

            order.get(key).cloned().unwrap_or_default()
        };

        let mut messages = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(message) = self.read(key, &id).await? {
                messages.push(message);
            }
        }

        Ok(messages)
    }

    async fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        match key {
            Some(key) => {
                let directory = self.directory_for(key)?;

                match fs::remove_dir_all(&directory).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(io_failure("remove the mailbox of", &error)),
                }

                let mut order = self.order.write().map_err(|_| poisoned())?;
                let _removed = order.remove(key);
            }
            None => {
                // The root itself is kept: it is frequently a mount point, and removing
                // it would leave the server writing into a directory that no longer
                // exists.
                let mut directories = fs::read_dir(&self.root)
                    .await
                    .map_err(|error| io_failure("list the data directory of", &error))?;

                while let Some(entry) = directories
                    .next_entry()
                    .await
                    .map_err(|error| io_failure("list the data directory of", &error))?
                {
                    if entry
                        .file_type()
                        .await
                        .map_err(|error| io_failure("inspect", &error))?
                        .is_dir()
                    {
                        fs::remove_dir_all(entry.path())
                            .await
                            .map_err(|error| io_failure("remove a mailbox of", &error))?;
                    }
                }

                let mut order = self.order.write().map_err(|_| poisoned())?;
                order.clear();
            }
        }

        Ok(())
    }

    async fn in_flight(&self) -> Result<Vec<Message>> {
        let keys: Vec<ApiKey> = {
            let order = self.order.read().map_err(|_| poisoned())?;

            order.keys().cloned().collect()
        };

        let mut messages = Vec::new();
        for key in keys {
            messages.extend(
                self.list(&key)
                    .await?
                    .into_iter()
                    .filter(|message| !message.last_event.is_terminal()),
            );
        }

        Ok(messages)
    }
}

#[cfg(test)]
mod disk_store {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use bytes::Bytes;
    use chrono::{DateTime, Duration, Utc};

    use super::DiskStore;
    use crate::events::EmailEvent;
    use crate::model::{ApiKey, Attachment, AttachmentSource, EmailId, Message};
    use crate::store::Store;

    /// A temporary directory that cleans itself up.
    ///
    /// Hand-rolled rather than pulling in a crate for it: this is the only place in the
    /// workspace that needs one, and it is six lines.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("mock-resend-{name}-{}", EmailId::generate()));

            std::fs::create_dir_all(&path).expect("the temporary directory is creatable");

            Self(path)
        }

        fn path(&self) -> &PathBuf {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _removed = std::fs::remove_dir_all(&self.0);
        }
    }

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn message(key: &ApiKey, subject: &str, created_at: DateTime<Utc>) -> Message {
        Message {
            id: EmailId::generate(),
            key: key.clone(),
            from: "Acme <onboarding@example.com>".to_owned(),
            to: vec!["delivered@example.org".to_owned()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: subject.to_owned(),
            html: None,
            text: Some("It works".to_owned()),
            headers: BTreeMap::new(),
            attachments: Vec::new(),
            tags: Vec::new(),
            template: None,
            scheduled_at: None,
            created_at,
            last_event: EmailEvent::Sent,
            message_id: Some("<abc@mock-resend.invalid>".to_owned()),
        }
    }

    fn at(offset: i64) -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + Duration::seconds(offset)
    }

    #[tokio::test]
    async fn should_return_a_message_it_captured() {
        let directory = TempDir::new("captured");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_return_a_captured_message_disk");
        let message = message(&key, "hello", at(0));

        store.put(message.clone()).await.expect("put succeeds");

        let found = store
            .get(&key, &message.id)
            .await
            .expect("get succeeds")
            .expect("the message is there");

        assert_eq!(found, message);
    }

    #[tokio::test]
    async fn should_write_one_eml_per_message() {
        let directory = TempDir::new("one-eml");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_write_one_eml_per_message");

        for subject in ["one", "two"] {
            store
                .put(message(&key, subject, at(0)))
                .await
                .expect("put succeeds");
        }

        let mailbox = std::fs::read_dir(directory.path())
            .expect("the root is readable")
            .next()
            .expect("there is a mailbox")
            .expect("it is readable")
            .path();

        let files: Vec<_> = std::fs::read_dir(&mailbox)
            .expect("the mailbox is readable")
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "eml"))
            .collect();

        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn should_write_a_file_a_mail_client_can_open() {
        let directory = TempDir::new("openable");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_write_an_openable_file");

        store
            .put(message(&key, "Hello World", at(0)))
            .await
            .expect("put succeeds");

        let mailbox = std::fs::read_dir(directory.path())
            .expect("readable")
            .next()
            .expect("a mailbox")
            .expect("readable")
            .path();
        let file = std::fs::read_dir(&mailbox)
            .expect("readable")
            .next()
            .expect("a file")
            .expect("readable")
            .path();

        let contents = std::fs::read_to_string(&file).expect("it is text");

        assert!(contents.starts_with("From:"));
        assert!(contents.contains("Subject: Hello World"));
    }

    #[tokio::test]
    async fn should_not_return_a_message_belonging_to_another_key() {
        let directory = TempDir::new("isolation");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let mine = key("re_should_not_leak_across_keys_disk_mine");
        let theirs = key("re_should_not_leak_across_keys_disk_theirs");
        let message = message(&mine, "private", at(0));

        store.put(message.clone()).await.expect("put succeeds");

        assert!(
            store
                .get(&theirs, &message.id)
                .await
                .expect("get succeeds")
                .is_none()
        );
    }

    #[tokio::test]
    async fn should_give_two_keys_that_sanitise_alike_separate_mailboxes() {
        let directory = TempDir::new("collision");
        let store = DiskStore::open(directory.path()).await.expect("it opens");

        // Both of these sanitise to the same filename. Sharing a directory would mean
        // sharing a mailbox, which is the one thing this project cannot get wrong.
        let first = key("re_a/b");
        let second = key("re_a_b");

        let mine = message(&first, "mine", at(0));
        store.put(mine.clone()).await.expect("put succeeds");
        store
            .put(message(&second, "theirs", at(0)))
            .await
            .expect("put succeeds");

        assert!(
            store
                .get(&second, &mine.id)
                .await
                .expect("get succeeds")
                .is_none(),
            "two keys landed in one mailbox"
        );
        assert_eq!(store.list(&first).await.expect("list").len(), 1);
        assert_eq!(store.list(&second).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn should_not_let_an_api_key_escape_the_data_directory() {
        let directory = TempDir::new("traversal");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let hostile = key("re_../../../../etc/passwd");

        store
            .put(message(&hostile, "nope", at(0)))
            .await
            .expect("put succeeds");

        // Everything written is a direct child of the root, whatever the key looked like.
        let children: Vec<_> = std::fs::read_dir(directory.path())
            .expect("readable")
            .filter_map(std::result::Result::ok)
            .collect();

        assert_eq!(children.len(), 1);
        assert_eq!(
            children[0].path().parent(),
            Some(directory.path().as_path())
        );
    }

    #[tokio::test]
    async fn should_list_messages_oldest_first() {
        let directory = TempDir::new("order");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_list_oldest_first_disk");

        for (offset, subject) in [(0, "first"), (1, "second"), (2, "third")] {
            store
                .put(message(&key, subject, at(offset)))
                .await
                .expect("put succeeds");
        }

        let subjects: Vec<String> = store
            .list(&key)
            .await
            .expect("list")
            .into_iter()
            .map(|message| message.subject)
            .collect();

        assert_eq!(subjects, ["first", "second", "third"]);
    }

    #[tokio::test]
    async fn should_replace_a_message_rather_than_duplicating_it() {
        let directory = TempDir::new("replace");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_replace_not_duplicate_disk");

        let mut message = message(&key, "hello", at(0));
        store.put(message.clone()).await.expect("put succeeds");

        message.last_event = EmailEvent::Delivered;
        store.put(message.clone()).await.expect("put succeeds");

        let listed = store.list(&key).await.expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].last_event, EmailEvent::Delivered);
    }

    #[tokio::test]
    async fn should_find_captured_mail_again_after_a_restart() {
        let directory = TempDir::new("restart");
        let key = key("re_should_survive_a_restart");
        let message = {
            let store = DiskStore::open(directory.path()).await.expect("it opens");
            let message = message(&key, "hello", at(0));

            store.put(message.clone()).await.expect("put succeeds");

            message
        };

        // A second store over the same directory is what a container restart looks like.
        let restarted = DiskStore::open(directory.path()).await.expect("it reopens");

        assert_eq!(
            restarted
                .get(&key, &message.id)
                .await
                .expect("get succeeds"),
            Some(message)
        );
        assert_eq!(restarted.list(&key).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn should_restore_the_original_order_after_a_restart() {
        let directory = TempDir::new("restart-order");
        let key = key("re_should_restore_order_after_restart");

        {
            let store = DiskStore::open(directory.path()).await.expect("it opens");

            for (offset, subject) in [(0, "first"), (1, "second"), (2, "third")] {
                store
                    .put(message(&key, subject, at(offset)))
                    .await
                    .expect("put succeeds");
            }
        }

        let restarted = DiskStore::open(directory.path()).await.expect("it reopens");

        let subjects: Vec<String> = restarted
            .list(&key)
            .await
            .expect("list")
            .into_iter()
            .map(|message| message.subject)
            .collect();

        assert_eq!(subjects, ["first", "second", "third"]);
    }

    #[tokio::test]
    async fn should_ignore_a_file_it_did_not_write() {
        let directory = TempDir::new("foreign");
        let key = key("re_should_ignore_a_foreign_file");

        {
            let store = DiskStore::open(directory.path()).await.expect("it opens");
            store
                .put(message(&key, "mine", at(0)))
                .await
                .expect("put succeeds");
        }

        // A developer poking around in a mounted volume must not be able to stop the
        // server from starting.
        let mailbox = std::fs::read_dir(directory.path())
            .expect("readable")
            .next()
            .expect("a mailbox")
            .expect("readable")
            .path();
        std::fs::write(mailbox.join("notes.eml"), b"not a message at all")
            .expect("the file is writable");

        let restarted = DiskStore::open(directory.path())
            .await
            .expect("a foreign file is not fatal");

        assert_eq!(restarted.list(&key).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn should_discard_only_the_key_it_was_asked_to_reset() {
        let directory = TempDir::new("reset-one");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let reset = key("re_should_discard_only_the_named_key_disk");
        let kept = key("re_should_keep_the_other_key_disk");

        store
            .put(message(&reset, "goes", at(0)))
            .await
            .expect("put");
        store
            .put(message(&kept, "stays", at(0)))
            .await
            .expect("put");

        store.reset(Some(&reset)).await.expect("reset succeeds");

        assert!(store.list(&reset).await.expect("list").is_empty());
        assert_eq!(store.list(&kept).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn should_keep_the_data_directory_when_resetting_everything() {
        let directory = TempDir::new("reset-all");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_keep_the_root_on_reset");

        store.put(message(&key, "goes", at(0))).await.expect("put");

        store.reset(None).await.expect("reset succeeds");

        assert!(
            directory.path().exists(),
            "the root is often a mount point; removing it would leave nowhere to write"
        );
        assert!(store.list(&key).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn should_accept_a_key_again_after_it_was_reset() {
        let directory = TempDir::new("reset-reuse");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_accept_a_key_again_disk");

        store
            .put(message(&key, "before", at(0)))
            .await
            .expect("put");
        store.reset(Some(&key)).await.expect("reset");
        store.put(message(&key, "after", at(1))).await.expect("put");

        let listed = store.list(&key).await.expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].subject, "after");
    }

    #[tokio::test]
    async fn should_report_only_messages_that_are_still_moving() {
        let directory = TempDir::new("in-flight");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_report_in_flight_disk");

        let mut moving = message(&key, "moving", at(0));
        moving.last_event = EmailEvent::Sent;
        let mut finished = message(&key, "finished", at(1));
        finished.last_event = EmailEvent::Bounced;

        store.put(moving.clone()).await.expect("put");
        store.put(finished).await.expect("put");

        let in_flight = store.in_flight().await.expect("in_flight");

        assert_eq!(in_flight.len(), 1);
        assert_eq!(in_flight[0].id, moving.id);
    }

    #[tokio::test]
    async fn should_open_a_data_directory_that_does_not_exist_yet() {
        let directory = TempDir::new("nested");
        let nested = directory.path().join("a").join("b");

        let store = DiskStore::open(&nested)
            .await
            .expect("it creates the directory");

        assert_eq!(store.root(), nested.as_path());
        assert!(nested.exists());
    }
    #[tokio::test]
    async fn should_return_none_for_an_id_that_was_never_captured() {
        let directory = TempDir::new("unknown-id");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_return_none_for_an_unknown_id_disk");

        assert!(
            store
                .get(&key, &EmailId::generate())
                .await
                .expect("get succeeds")
                .is_none()
        );
    }

    #[tokio::test]
    async fn should_list_nothing_for_a_key_that_has_never_been_used() {
        let directory = TempDir::new("unused-key");
        let store = DiskStore::open(directory.path()).await.expect("it opens");

        assert!(
            store
                .list(&key("re_should_list_nothing_for_an_unused_key_disk"))
                .await
                .expect("list succeeds")
                .is_empty()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn should_keep_every_message_when_one_key_is_written_to_concurrently() {
        // A parallel suite under one key is the normal case, and the mailbox directory
        // is created by whichever write gets there first. A lost message or a lost index
        // entry here would look like mail that was captured and then vanished.
        let directory = TempDir::new("concurrent");
        let store = std::sync::Arc::new(DiskStore::open(directory.path()).await.expect("it opens"));
        let key = key("re_should_capture_concurrently_disk");

        let messages: Vec<Message> = (0..16)
            .map(|index| message(&key, &format!("message {index}"), at(index)))
            .collect();

        let writes: Vec<_> = messages
            .iter()
            .cloned()
            .map(|message| {
                let store = std::sync::Arc::clone(&store);

                tokio::spawn(async move { store.put(message).await })
            })
            .collect();

        for write in writes {
            write
                .await
                .expect("the task finished")
                .expect("put succeeds");
        }

        let listed = store.list(&key).await.expect("list succeeds");

        assert_eq!(listed.len(), messages.len());

        for message in &messages {
            assert_eq!(
                listed.iter().filter(|found| found.id == message.id).count(),
                1,
                "every message is listed exactly once"
            );
        }
    }

    #[tokio::test]
    async fn should_keep_an_attachment_whose_filename_looks_like_a_path_inside_the_mailbox() {
        // The filename is a caller-supplied string, and clients send this one. It is
        // part of the message rather than part of the path, so it survives untouched
        // and nothing is written outside the data directory.
        let directory = TempDir::new("hostile-attachment");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let key = key("re_should_keep_a_hostile_attachment_filename");

        let mut message = message(&key, "Attached", at(0));
        message.attachments = vec![Attachment {
            filename: Some("../../../../etc/passwd".to_owned()),
            content_type: Some("text/plain".to_owned()),
            content_id: None,
            source: AttachmentSource::Content(Bytes::from_static(b"not a password file")),
        }];

        store.put(message.clone()).await.expect("put succeeds");

        let children: Vec<_> = std::fs::read_dir(directory.path())
            .expect("the root is readable")
            .filter_map(std::result::Result::ok)
            .collect();

        assert_eq!(children.len(), 1, "only the mailbox directory is written");

        let files: Vec<_> = std::fs::read_dir(children[0].path())
            .expect("the mailbox is readable")
            .filter_map(std::result::Result::ok)
            .collect();

        assert_eq!(
            files.len(),
            1,
            "one .eml, and no file named after the filename"
        );

        let found = store
            .get(&key, &message.id)
            .await
            .expect("get succeeds")
            .expect("the message is there");

        assert_eq!(found.attachments, message.attachments);
    }

    #[tokio::test]
    async fn should_not_let_an_attachment_filename_rewrite_the_messages_own_state() {
        // A filename carrying a line break is an attempt to end the header it is written
        // into and start another. If one got through, a message could claim an owner or a
        // delivery event the engine never gave it.
        let directory = TempDir::new("header-injection");
        let store = DiskStore::open(directory.path()).await.expect("it opens");
        let owner = key("re_should_not_let_a_filename_rewrite_state");

        let mut message = message(&owner, "Injected", at(0));
        message.attachments = vec![Attachment {
            filename: Some(
                "report.csv\r\nX-Mock-Resend-Api-Key: re_forged\r\nX-Mock-Resend-Last-Event: delivered"
                    .to_owned(),
            ),
            content_type: Some("text/csv".to_owned()),
            content_id: None,
            source: AttachmentSource::Content(Bytes::from_static(b"name,count")),
        }];

        store.put(message.clone()).await.expect("put succeeds");

        let found = store
            .get(&owner, &message.id)
            .await
            .expect("get succeeds")
            .expect("the message is there");

        assert_eq!(found.key, owner);
        assert_eq!(found.last_event, EmailEvent::Sent);
        assert!(
            store
                .list(&key("re_forged"))
                .await
                .expect("list succeeds")
                .is_empty()
        );
    }
}
