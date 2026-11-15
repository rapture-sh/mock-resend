//! The in-memory backend.
//!
//! Captured mail lives in a map and goes away with the process, which is what a test
//! suite wants. It is also the only backend so far: the disk backend writes an `.eml`,
//! so it lands with the MIME writer rather than ahead of it.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::{Error, Result};
use crate::model::{ApiKey, EmailId, Message};
use crate::store::Store;

/// One API key's mail, in the order it arrived.
#[derive(Debug, Default)]
struct Namespace {
    /// Ids oldest first, so `list` does not have to sort by a timestamp that a test
    /// clock may have handed out more than once.
    order: Vec<EmailId>,
    messages: HashMap<EmailId, Message>,
}

impl Namespace {
    fn insert(&mut self, message: Message) {
        let id = message.id;

        // Only a first capture extends the order; a replacement keeps its place, so an
        // event transition does not reorder the mailbox under a caller's feet.
        if self.messages.insert(id, message).is_none() {
            self.order.push(id);
        }
    }
}

/// A [`Store`] holding everything in memory.
#[derive(Debug, Default)]
pub struct MemoryStore {
    /// Keyed by API key, because every namespace is independent.
    ///
    /// A `std` lock rather than a `tokio` one: the critical sections are map operations
    /// with no `await` inside them, so the runtime is never blocked on I/O here.
    namespaces: RwLock<HashMap<ApiKey, Namespace>>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Turns a poisoned lock into a storage failure.
///
/// A panic while holding the lock means a bug, not a condition a caller can act on, so
/// it surfaces as `application_error` rather than taking the process down.
fn poisoned() -> Error {
    Error::Store("the in-memory store's lock was poisoned by an earlier panic".to_owned())
}

#[async_trait]
impl Store for MemoryStore {
    async fn put(&self, message: Message) -> Result<()> {
        let mut namespaces = self.namespaces.write().map_err(|_| poisoned())?;

        namespaces
            .entry(message.key.clone())
            .or_default()
            .insert(message);

        Ok(())
    }

    async fn get(&self, key: &ApiKey, id: &EmailId) -> Result<Option<Message>> {
        let namespaces = self.namespaces.read().map_err(|_| poisoned())?;

        Ok(namespaces
            .get(key)
            .and_then(|namespace| namespace.messages.get(id))
            .cloned())
    }

    async fn list(&self, key: &ApiKey) -> Result<Vec<Message>> {
        let namespaces = self.namespaces.read().map_err(|_| poisoned())?;

        let Some(namespace) = namespaces.get(key) else {
            return Ok(Vec::new());
        };

        Ok(namespace
            .order
            .iter()
            .filter_map(|id| namespace.messages.get(id))
            .cloned()
            .collect())
    }

    async fn reset(&self, key: Option<&ApiKey>) -> Result<()> {
        let mut namespaces = self.namespaces.write().map_err(|_| poisoned())?;

        match key {
            Some(key) => {
                let _removed = namespaces.remove(key);
            }
            None => namespaces.clear(),
        }

        Ok(())
    }

    async fn due_scheduled(&self, now: DateTime<Utc>) -> Result<Vec<Message>> {
        let namespaces = self.namespaces.read().map_err(|_| poisoned())?;

        Ok(namespaces
            .values()
            .flat_map(|namespace| namespace.messages.values())
            .filter(|message| message.last_event.is_scheduled())
            .filter(|message| {
                message
                    .scheduled_at
                    .is_some_and(|scheduled_at| scheduled_at <= now)
            })
            .cloned()
            .collect())
    }
}

#[cfg(test)]
mod memory_store {
    use std::collections::BTreeMap;

    use chrono::{DateTime, Utc};

    use super::MemoryStore;
    use crate::events::EmailEvent;
    use crate::model::{ApiKey, EmailId, Message};
    use crate::store::Store;

    fn key(name: &str) -> ApiKey {
        ApiKey::parse(name).expect("the test key is re_-prefixed")
    }

    fn message(key: &ApiKey, subject: &str) -> Message {
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
            text: Some("body".to_owned()),
            headers: BTreeMap::new(),
            attachments: Vec::new(),
            tags: Vec::new(),
            template: None,
            scheduled_at: None,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            last_event: EmailEvent::Sent,
            message_id: None,
        }
    }

    #[tokio::test]
    async fn should_return_a_message_it_captured() {
        let store = MemoryStore::new();
        let key = key("re_should_return_a_message_it_captured");
        let message = message(&key, "hello");

        store.put(message.clone()).await.expect("put succeeds");

        let found = store
            .get(&key, &message.id)
            .await
            .expect("get succeeds")
            .expect("the message is there");

        assert_eq!(found, message);
    }

    #[tokio::test]
    async fn should_return_none_for_an_id_that_was_never_captured() {
        let store = MemoryStore::new();
        let key = key("re_should_return_none_for_an_unknown_id");

        let found = store
            .get(&key, &EmailId::generate())
            .await
            .expect("get succeeds");

        assert!(found.is_none());
    }

    #[tokio::test]
    async fn should_not_return_a_message_belonging_to_another_key() {
        let store = MemoryStore::new();
        let mine = key("re_should_not_leak_mine");
        let theirs = key("re_should_not_leak_theirs");
        let message = message(&mine, "private");

        store.put(message.clone()).await.expect("put succeeds");

        assert!(
            store
                .get(&theirs, &message.id)
                .await
                .expect("get succeeds")
                .is_none(),
            "a message read across keys would make a parallel suite non-deterministic"
        );
    }

    #[tokio::test]
    async fn should_list_messages_oldest_first() {
        let store = MemoryStore::new();
        let key = key("re_should_list_messages_oldest_first");

        for subject in ["first", "second", "third"] {
            store
                .put(message(&key, subject))
                .await
                .expect("put succeeds");
        }

        let subjects: Vec<String> = store
            .list(&key)
            .await
            .expect("list succeeds")
            .into_iter()
            .map(|message| message.subject)
            .collect();

        assert_eq!(subjects, ["first", "second", "third"]);
    }

    #[tokio::test]
    async fn should_list_nothing_for_a_key_that_has_never_been_used() {
        let store = MemoryStore::new();

        let listed = store
            .list(&key("re_should_list_nothing_for_an_unused_key"))
            .await
            .expect("list succeeds");

        assert!(listed.is_empty());
    }

    #[tokio::test]
    async fn should_keep_a_replaced_message_in_its_original_position() {
        let store = MemoryStore::new();
        let key = key("re_should_keep_a_replaced_message_in_place");

        let first = message(&key, "first");
        let second = message(&key, "second");
        store.put(first.clone()).await.expect("put succeeds");
        store.put(second).await.expect("put succeeds");

        let mut updated = first.clone();
        updated.last_event = crate::events::EmailEvent::Delivered;
        store.put(updated).await.expect("put succeeds");

        let listed = store.list(&key).await.expect("list succeeds");

        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, first.id);
        assert_eq!(listed[0].last_event, crate::events::EmailEvent::Delivered);
    }

    #[tokio::test]
    async fn should_discard_only_the_key_it_was_asked_to_reset() {
        let store = MemoryStore::new();
        let reset = key("re_should_discard_only_the_named_key_reset");
        let kept = key("re_should_discard_only_the_named_key_kept");

        store.put(message(&reset, "goes")).await.expect("put");
        store.put(message(&kept, "stays")).await.expect("put");

        store.reset(Some(&reset)).await.expect("reset succeeds");

        assert!(store.list(&reset).await.expect("list").is_empty());
        assert_eq!(store.list(&kept).await.expect("list").len(), 1);
    }

    #[tokio::test]
    async fn should_discard_every_key_when_reset_without_one() {
        let store = MemoryStore::new();
        let first = key("re_should_discard_every_key_first");
        let second = key("re_should_discard_every_key_second");

        store.put(message(&first, "a")).await.expect("put");
        store.put(message(&second, "b")).await.expect("put");

        store.reset(None).await.expect("reset succeeds");

        assert!(store.list(&first).await.expect("list").is_empty());
        assert!(store.list(&second).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn should_accept_a_key_again_after_it_was_reset() {
        let store = MemoryStore::new();
        let key = key("re_should_accept_a_key_again_after_reset");

        store.put(message(&key, "before")).await.expect("put");
        store.reset(Some(&key)).await.expect("reset");
        store.put(message(&key, "after")).await.expect("put");

        let listed = store.list(&key).await.expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].subject, "after");
    }
}
