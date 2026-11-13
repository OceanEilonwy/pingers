// SPDX-License-Identifier: AGPL-3.0-only
//! Chat threads, one level deep (design B.6): a reply always points at a root message.

use std::collections::BTreeMap;

use pingers_proto::ChatMessageId;

/// Which messages are known and which root each belongs to. Holds ids only; bodies go to the
/// caller in events and are stored there.
#[derive(Clone, Debug, Default)]
pub(crate) struct Threads {
    /// Each known message and its root (`None` for a root).
    parents: BTreeMap<ChatMessageId, Option<ChatMessageId>>,
}

/// Whether a message was new.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recorded {
    /// First time seen; carries its (flattened) root.
    New { parent: Option<ChatMessageId> },
    /// Already seen; ignore the duplicate.
    Duplicate,
}

impl Threads {
    /// The root a reply to `parent` belongs under: a reply to a reply goes under the reply's
    /// root, so threads stay one level deep. An unknown parent (sent before this device
    /// joined) is kept as given.
    pub(crate) fn root_for(&self, parent: ChatMessageId) -> ChatMessageId {
        match self.parents.get(&parent) {
            Some(Some(root)) => *root,
            Some(None) | None => parent,
        }
    }

    /// Records a message, flattening its parent to a root.
    pub(crate) fn record(&mut self, id: ChatMessageId, parent: Option<ChatMessageId>) -> Recorded {
        if self.parents.contains_key(&id) {
            return Recorded::Duplicate;
        }
        let parent = parent.map(|parent| self.root_for(parent));
        self.parents.insert(id, parent);
        Recorded::New { parent }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> ChatMessageId {
        ChatMessageId::from_bytes([byte; 16])
    }

    #[test]
    fn a_reply_to_a_reply_goes_under_the_root() {
        let mut threads = Threads::default();
        assert_eq!(threads.record(id(1), None), Recorded::New { parent: None });
        assert_eq!(
            threads.record(id(2), Some(id(1))),
            Recorded::New {
                parent: Some(id(1))
            }
        );
        assert_eq!(
            threads.record(id(3), Some(id(2))),
            Recorded::New {
                parent: Some(id(1))
            }
        );
    }

    #[test]
    fn a_reply_to_an_unknown_message_keeps_its_parent() {
        let mut threads = Threads::default();
        assert_eq!(
            threads.record(id(2), Some(id(9))),
            Recorded::New {
                parent: Some(id(9))
            }
        );
    }

    #[test]
    fn a_repeated_id_is_a_duplicate() {
        let mut threads = Threads::default();
        assert_eq!(threads.record(id(1), None), Recorded::New { parent: None });
        assert_eq!(threads.record(id(1), None), Recorded::Duplicate);
    }
}
