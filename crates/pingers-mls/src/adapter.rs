// SPDX-License-Identifier: AGPL-3.0-only
//! The adapter from [`MlsStorage`] to OpenMLS's `StorageProvider`.
//!
//! Key layout. Rows that belong to one group start with that group's prefix,
//! [`group_storage_prefix`]: `"openmls/v1/group/" || JSON(group_id) || "/"`, followed by
//! `label` (a row per group) or `label || "/" || JSON(item)` (a row per proposal, or per epoch
//! and leaf). Rows that belong to no group (KeyPackages, PSKs, standalone key pairs) are
//! `"openmls/v1/" || label || "/" || JSON(key)`. Labels are fixed ASCII names without `/`, and
//! JSON values are self-delimiting, so no two rows collide and no group's prefix is a prefix of
//! another's. Deleting everything under a group's prefix deletes all its state. Values are JSON,
//! as in OpenMLS's own memory storage; lists are a JSON array of JSON-encoded items.
//!
//! OpenMLS hides the provider's error inside its own error types, so the adapter also records
//! the first storage failure of an operation; the wrapper reports it as [`MlsError::Storage`]
//! whatever OpenMLS made of it.

use core::cell::Cell;

use openmls_traits::storage::{CURRENT_VERSION, StorageProvider, traits};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{MlsError, MlsStorage, StorageError};

const PREFIX: &[u8] = b"openmls/v1/";
const GROUP_PREFIX: &[u8] = b"openmls/v1/group/";

/// The prefix of every storage key that belongs to `group`, as OpenMLS names it.
fn openmls_group_prefix(group: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let mut key = GROUP_PREFIX.to_vec();
    key.extend_from_slice(&serde_json::to_vec(group)?);
    key.push(b'/');
    Ok(key)
}

/// The prefix of every storage key that belongs to the group `group_id`: OpenMLS's rows for it
/// and the wrapper's own. [`MlsStorage::delete_prefix`] with it deletes the group's state.
///
/// # Errors
///
/// [`MlsError::Internal`] if the group id cannot be encoded (it always can).
pub fn group_storage_prefix(group_id: &pingers_proto::GroupId) -> Result<Vec<u8>, MlsError> {
    openmls_group_prefix(&openmls::prelude::GroupId::from_slice(group_id.as_bytes()))
        .map_err(|_| MlsError::Internal)
}

/// How a row's storage key is formed.
trait KeySpec {
    fn storage_key(&self, label: &[u8]) -> Result<Vec<u8>, serde_json::Error>;
}

/// A row that belongs to no group.
struct Global<'key, K>(&'key K);

/// The one row of its label for a group.
struct InGroup<'key, G>(&'key G);

/// One of many rows of its label for a group.
struct GroupItem<'key, G, I>(&'key G, &'key I);

impl<K: Serialize> KeySpec for Global<'_, K> {
    fn storage_key(&self, label: &[u8]) -> Result<Vec<u8>, serde_json::Error> {
        Ok(raw_key(label, &serde_json::to_vec(self.0)?))
    }
}

impl<G: Serialize> KeySpec for InGroup<'_, G> {
    fn storage_key(&self, label: &[u8]) -> Result<Vec<u8>, serde_json::Error> {
        let mut key = openmls_group_prefix(self.0)?;
        key.extend_from_slice(label);
        Ok(key)
    }
}

impl<G: Serialize, I: Serialize> KeySpec for GroupItem<'_, G, I> {
    fn storage_key(&self, label: &[u8]) -> Result<Vec<u8>, serde_json::Error> {
        let mut key = openmls_group_prefix(self.0)?;
        key.extend_from_slice(label);
        key.push(b'/');
        key.extend_from_slice(&serde_json::to_vec(self.1)?);
        Ok(key)
    }
}

const KEY_PACKAGE: &[u8] = b"KeyPackage";
const PSK: &[u8] = b"Psk";
const ENCRYPTION_KEY_PAIR: &[u8] = b"EncryptionKeyPair";
const SIGNATURE_KEY_PAIR: &[u8] = b"SignatureKeyPair";
const EPOCH_KEY_PAIRS: &[u8] = b"EpochKeyPairs";
const TREE: &[u8] = b"Tree";
const GROUP_CONTEXT: &[u8] = b"GroupContext";
const INTERIM_TRANSCRIPT_HASH: &[u8] = b"InterimTranscriptHash";
const CONFIRMATION_TAG: &[u8] = b"ConfirmationTag";
const JOIN_CONFIG: &[u8] = b"MlsGroupJoinConfig";
const OWN_LEAF_NODES: &[u8] = b"OwnLeafNodes";
const GROUP_STATE: &[u8] = b"GroupState";
const QUEUED_PROPOSAL: &[u8] = b"QueuedProposal";
const PROPOSAL_QUEUE_REFS: &[u8] = b"ProposalQueueRefs";
const OWN_LEAF_NODE_INDEX: &[u8] = b"OwnLeafNodeIndex";
const EPOCH_SECRETS: &[u8] = b"EpochSecrets";
const RESUMPTION_PSK_STORE: &[u8] = b"ResumptionPsk";
const MESSAGE_SECRETS: &[u8] = b"MessageSecrets";

/// [`MlsStorage`] seen as an OpenMLS storage provider for the length of one operation.
pub(crate) struct StorageAdapter<'storage, S: MlsStorage> {
    storage: &'storage S,
    failure: Cell<Option<StorageError>>,
}

impl<'storage, S: MlsStorage> StorageAdapter<'storage, S> {
    pub(crate) const fn new(storage: &'storage S) -> Self {
        Self {
            storage,
            failure: Cell::new(None),
        }
    }

    /// `fallback`, unless a storage call failed during the operation: then that failure.
    pub(crate) fn error_or(&self, fallback: MlsError) -> MlsError {
        match self.failure.get() {
            Some(failure) => MlsError::Storage(failure),
            None => fallback,
        }
    }

    fn record<T>(&self, result: Result<T, StorageError>) -> Result<T, StorageError> {
        if let Err(failure) = &result
            && self.failure.get().is_none()
        {
            self.failure.set(Some(*failure));
        }
        result
    }

    fn storage_key(&self, label: &[u8], key: &impl KeySpec) -> Result<Vec<u8>, StorageError> {
        self.record(key.storage_key(label).map_err(|_| StorageError::Corrupt))
    }

    fn encode(&self, value: &impl Serialize) -> Result<Vec<u8>, StorageError> {
        self.record(serde_json::to_vec(value).map_err(|_| StorageError::Corrupt))
    }

    fn decode<V: DeserializeOwned>(&self, bytes: &[u8]) -> Result<V, StorageError> {
        self.record(serde_json::from_slice(bytes).map_err(|_| StorageError::Corrupt))
    }

    fn put(
        &self,
        label: &[u8],
        key: &impl KeySpec,
        value: &impl Serialize,
    ) -> Result<(), StorageError> {
        let key = self.storage_key(label, key)?;
        let value = self.encode(value)?;
        self.record(self.storage.write(&key, &value))
    }

    fn get<V: DeserializeOwned>(
        &self,
        label: &[u8],
        key: &impl KeySpec,
    ) -> Result<Option<V>, StorageError> {
        let key = self.storage_key(label, key)?;
        match self.record(self.storage.read(&key))? {
            Some(bytes) => self.decode(&bytes).map(Some),
            None => Ok(None),
        }
    }

    fn remove(&self, label: &[u8], key: &impl KeySpec) -> Result<(), StorageError> {
        let key = self.storage_key(label, key)?;
        self.record(self.storage.delete(&key))
    }

    fn raw_list(&self, label: &[u8], key: &impl KeySpec) -> Result<Vec<Vec<u8>>, StorageError> {
        Ok(self.get::<Vec<Vec<u8>>>(label, key)?.unwrap_or_default())
    }

    fn get_list<V: DeserializeOwned>(
        &self,
        label: &[u8],
        key: &impl KeySpec,
    ) -> Result<Vec<V>, StorageError> {
        self.raw_list(label, key)?
            .iter()
            .map(|item| self.decode(item))
            .collect()
    }

    fn append(
        &self,
        label: &[u8],
        key: &impl KeySpec,
        item: &impl Serialize,
    ) -> Result<(), StorageError> {
        let mut list = self.raw_list(label, key)?;
        list.push(self.encode(item)?);
        self.put(label, key, &list)
    }

    fn remove_from_list(
        &self,
        label: &[u8],
        key: &impl KeySpec,
        item: &impl Serialize,
    ) -> Result<(), StorageError> {
        let mut list = self.raw_list(label, key)?;
        let item = self.encode(item)?;
        list.retain(|stored| *stored != item);
        self.put(label, key, &list)
    }
}

/// The storage key for an already encoded OpenMLS key.
fn raw_key(label: &[u8], encoded_key: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(
        PREFIX
            .len()
            .saturating_add(label.len())
            .saturating_add(1)
            .saturating_add(encoded_key.len()),
    );
    key.extend_from_slice(PREFIX);
    key.extend_from_slice(label);
    key.push(b'/');
    key.extend_from_slice(encoded_key);
    key
}

/// The item key of a group's encryption key pairs for one epoch and leaf.
#[derive(Serialize)]
struct EpochLeaf<'key, E> {
    epoch: &'key E,
    leaf: u32,
}

impl<S: MlsStorage> StorageProvider<CURRENT_VERSION> for StorageAdapter<'_, S> {
    type Error = StorageError;

    fn write_mls_join_config<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        config: &MlsGroupJoinConfig,
    ) -> Result<(), Self::Error> {
        self.put(JOIN_CONFIG, &InGroup(group_id), config)
    }

    fn append_own_leaf_node<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNode: traits::LeafNode<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        leaf_node: &LeafNode,
    ) -> Result<(), Self::Error> {
        self.append(OWN_LEAF_NODES, &InGroup(group_id), leaf_node)
    }

    fn queue_proposal<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
        QueuedProposal: traits::QueuedProposal<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
        proposal: &QueuedProposal,
    ) -> Result<(), Self::Error> {
        let key = GroupItem(group_id, proposal_ref);
        self.put(QUEUED_PROPOSAL, &key, proposal)?;
        self.append(PROPOSAL_QUEUE_REFS, &InGroup(group_id), proposal_ref)
    }

    fn write_tree<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        TreeSync: traits::TreeSync<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        tree: &TreeSync,
    ) -> Result<(), Self::Error> {
        self.put(TREE, &InGroup(group_id), tree)
    }

    fn write_interim_transcript_hash<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        interim_transcript_hash: &InterimTranscriptHash,
    ) -> Result<(), Self::Error> {
        self.put(
            INTERIM_TRANSCRIPT_HASH,
            &InGroup(group_id),
            interim_transcript_hash,
        )
    }

    fn write_context<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupContext: traits::GroupContext<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_context: &GroupContext,
    ) -> Result<(), Self::Error> {
        self.put(GROUP_CONTEXT, &InGroup(group_id), group_context)
    }

    fn write_confirmation_tag<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        confirmation_tag: &ConfirmationTag,
    ) -> Result<(), Self::Error> {
        self.put(CONFIRMATION_TAG, &InGroup(group_id), confirmation_tag)
    }

    fn write_group_state<
        GroupState: traits::GroupState<CURRENT_VERSION>,
        GroupId: traits::GroupId<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_state: &GroupState,
    ) -> Result<(), Self::Error> {
        self.put(GROUP_STATE, &InGroup(group_id), group_state)
    }

    fn write_message_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MessageSecrets: traits::MessageSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        message_secrets: &MessageSecrets,
    ) -> Result<(), Self::Error> {
        self.put(MESSAGE_SECRETS, &InGroup(group_id), message_secrets)
    }

    fn write_resumption_psk_store<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        resumption_psk_store: &ResumptionPskStore,
    ) -> Result<(), Self::Error> {
        self.put(
            RESUMPTION_PSK_STORE,
            &InGroup(group_id),
            resumption_psk_store,
        )
    }

    fn write_own_leaf_index<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        own_leaf_index: &LeafNodeIndex,
    ) -> Result<(), Self::Error> {
        self.put(OWN_LEAF_NODE_INDEX, &InGroup(group_id), own_leaf_index)
    }

    fn write_group_epoch_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        group_epoch_secrets: &GroupEpochSecrets,
    ) -> Result<(), Self::Error> {
        self.put(EPOCH_SECRETS, &InGroup(group_id), group_epoch_secrets)
    }

    fn write_signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
        signature_key_pair: &SignatureKeyPair,
    ) -> Result<(), Self::Error> {
        self.put(SIGNATURE_KEY_PAIR, &Global(public_key), signature_key_pair)
    }

    fn write_encryption_key_pair<
        EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &EncryptionKey,
        key_pair: &HpkeKeyPair,
    ) -> Result<(), Self::Error> {
        self.put(ENCRYPTION_KEY_PAIR, &Global(public_key), key_pair)
    }

    fn write_encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
        key_pairs: &[HpkeKeyPair],
    ) -> Result<(), Self::Error> {
        let key = GroupItem(
            group_id,
            &EpochLeaf {
                epoch,
                leaf: leaf_index,
            },
        );
        self.put(EPOCH_KEY_PAIRS, &key, &key_pairs)
    }

    fn write_key_package<
        HashReference: traits::HashReference<CURRENT_VERSION>,
        KeyPackage: traits::KeyPackage<CURRENT_VERSION>,
    >(
        &self,
        hash_ref: &HashReference,
        key_package: &KeyPackage,
    ) -> Result<(), Self::Error> {
        self.put(KEY_PACKAGE, &Global(hash_ref), key_package)
    }

    fn write_psk<
        PskId: traits::PskId<CURRENT_VERSION>,
        PskBundle: traits::PskBundle<CURRENT_VERSION>,
    >(
        &self,
        psk_id: &PskId,
        psk: &PskBundle,
    ) -> Result<(), Self::Error> {
        self.put(PSK, &Global(psk_id), psk)
    }

    fn mls_group_join_config<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MlsGroupJoinConfig: traits::MlsGroupJoinConfig<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MlsGroupJoinConfig>, Self::Error> {
        self.get(JOIN_CONFIG, &InGroup(group_id))
    }

    fn own_leaf_nodes<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNode: traits::LeafNode<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<LeafNode>, Self::Error> {
        self.get_list(OWN_LEAF_NODES, &InGroup(group_id))
    }

    fn queued_proposal_refs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<ProposalRef>, Self::Error> {
        self.get_list(PROPOSAL_QUEUE_REFS, &InGroup(group_id))
    }

    fn queued_proposals<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
        QueuedProposal: traits::QueuedProposal<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Vec<(ProposalRef, QueuedProposal)>, Self::Error> {
        let refs: Vec<ProposalRef> = self.get_list(PROPOSAL_QUEUE_REFS, &InGroup(group_id))?;
        refs.into_iter()
            .map(|proposal_ref| {
                let key = GroupItem(group_id, &proposal_ref);
                let proposal = self
                    .get(QUEUED_PROPOSAL, &key)?
                    .ok_or(StorageError::Corrupt);
                let proposal = self.record(proposal)?;
                Ok((proposal_ref, proposal))
            })
            .collect()
    }

    fn tree<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        TreeSync: traits::TreeSync<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<TreeSync>, Self::Error> {
        self.get(TREE, &InGroup(group_id))
    }

    fn group_context<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupContext: traits::GroupContext<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupContext>, Self::Error> {
        self.get(GROUP_CONTEXT, &InGroup(group_id))
    }

    fn interim_transcript_hash<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        InterimTranscriptHash: traits::InterimTranscriptHash<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<InterimTranscriptHash>, Self::Error> {
        self.get(INTERIM_TRANSCRIPT_HASH, &InGroup(group_id))
    }

    fn confirmation_tag<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ConfirmationTag: traits::ConfirmationTag<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ConfirmationTag>, Self::Error> {
        self.get(CONFIRMATION_TAG, &InGroup(group_id))
    }

    fn group_state<
        GroupState: traits::GroupState<CURRENT_VERSION>,
        GroupId: traits::GroupId<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupState>, Self::Error> {
        self.get(GROUP_STATE, &InGroup(group_id))
    }

    fn message_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        MessageSecrets: traits::MessageSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<MessageSecrets>, Self::Error> {
        self.get(MESSAGE_SECRETS, &InGroup(group_id))
    }

    fn resumption_psk_store<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ResumptionPskStore: traits::ResumptionPskStore<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<ResumptionPskStore>, Self::Error> {
        self.get(RESUMPTION_PSK_STORE, &InGroup(group_id))
    }

    fn own_leaf_index<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        LeafNodeIndex: traits::LeafNodeIndex<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<LeafNodeIndex>, Self::Error> {
        self.get(OWN_LEAF_NODE_INDEX, &InGroup(group_id))
    }

    fn group_epoch_secrets<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        GroupEpochSecrets: traits::GroupEpochSecrets<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<Option<GroupEpochSecrets>, Self::Error> {
        self.get(EPOCH_SECRETS, &InGroup(group_id))
    }

    fn signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
        SignatureKeyPair: traits::SignatureKeyPair<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<Option<SignatureKeyPair>, Self::Error> {
        self.get(SIGNATURE_KEY_PAIR, &Global(public_key))
    }

    fn encryption_key_pair<
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
        EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>,
    >(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<Option<HpkeKeyPair>, Self::Error> {
        self.get(ENCRYPTION_KEY_PAIR, &Global(public_key))
    }

    fn encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
        HpkeKeyPair: traits::HpkeKeyPair<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<Vec<HpkeKeyPair>, Self::Error> {
        let key = GroupItem(
            group_id,
            &EpochLeaf {
                epoch,
                leaf: leaf_index,
            },
        );
        Ok(self.get(EPOCH_KEY_PAIRS, &key)?.unwrap_or_default())
    }

    fn key_package<
        KeyPackageRef: traits::HashReference<CURRENT_VERSION>,
        KeyPackage: traits::KeyPackage<CURRENT_VERSION>,
    >(
        &self,
        hash_ref: &KeyPackageRef,
    ) -> Result<Option<KeyPackage>, Self::Error> {
        self.get(KEY_PACKAGE, &Global(hash_ref))
    }

    fn psk<PskBundle: traits::PskBundle<CURRENT_VERSION>, PskId: traits::PskId<CURRENT_VERSION>>(
        &self,
        psk_id: &PskId,
    ) -> Result<Option<PskBundle>, Self::Error> {
        self.get(PSK, &Global(psk_id))
    }

    fn remove_proposal<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        proposal_ref: &ProposalRef,
    ) -> Result<(), Self::Error> {
        self.remove_from_list(PROPOSAL_QUEUE_REFS, &InGroup(group_id), proposal_ref)?;
        let key = GroupItem(group_id, proposal_ref);
        self.remove(QUEUED_PROPOSAL, &key)
    }

    fn delete_own_leaf_nodes<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(OWN_LEAF_NODES, &InGroup(group_id))
    }

    fn delete_group_config<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(JOIN_CONFIG, &InGroup(group_id))
    }

    fn delete_tree<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(TREE, &InGroup(group_id))
    }

    fn delete_confirmation_tag<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(CONFIRMATION_TAG, &InGroup(group_id))
    }

    fn delete_group_state<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(GROUP_STATE, &InGroup(group_id))
    }

    fn delete_context<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(GROUP_CONTEXT, &InGroup(group_id))
    }

    fn delete_interim_transcript_hash<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(INTERIM_TRANSCRIPT_HASH, &InGroup(group_id))
    }

    fn delete_message_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(MESSAGE_SECRETS, &InGroup(group_id))
    }

    fn delete_all_resumption_psk_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(RESUMPTION_PSK_STORE, &InGroup(group_id))
    }

    fn delete_own_leaf_index<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(OWN_LEAF_NODE_INDEX, &InGroup(group_id))
    }

    fn delete_group_epoch_secrets<GroupId: traits::GroupId<CURRENT_VERSION>>(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        self.remove(EPOCH_SECRETS, &InGroup(group_id))
    }

    fn clear_proposal_queue<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        ProposalRef: traits::ProposalRef<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
    ) -> Result<(), Self::Error> {
        let refs: Vec<ProposalRef> = self.get_list(PROPOSAL_QUEUE_REFS, &InGroup(group_id))?;
        for proposal_ref in &refs {
            let key = GroupItem(group_id, proposal_ref);
            self.remove(QUEUED_PROPOSAL, &key)?;
        }
        self.remove(PROPOSAL_QUEUE_REFS, &InGroup(group_id))
    }

    fn delete_signature_key_pair<
        SignaturePublicKey: traits::SignaturePublicKey<CURRENT_VERSION>,
    >(
        &self,
        public_key: &SignaturePublicKey,
    ) -> Result<(), Self::Error> {
        self.remove(SIGNATURE_KEY_PAIR, &Global(public_key))
    }

    fn delete_encryption_key_pair<EncryptionKey: traits::EncryptionKey<CURRENT_VERSION>>(
        &self,
        public_key: &EncryptionKey,
    ) -> Result<(), Self::Error> {
        self.remove(ENCRYPTION_KEY_PAIR, &Global(public_key))
    }

    fn delete_encryption_epoch_key_pairs<
        GroupId: traits::GroupId<CURRENT_VERSION>,
        EpochKey: traits::EpochKey<CURRENT_VERSION>,
    >(
        &self,
        group_id: &GroupId,
        epoch: &EpochKey,
        leaf_index: u32,
    ) -> Result<(), Self::Error> {
        let key = GroupItem(
            group_id,
            &EpochLeaf {
                epoch,
                leaf: leaf_index,
            },
        );
        self.remove(EPOCH_KEY_PAIRS, &key)
    }

    fn delete_key_package<KeyPackageRef: traits::HashReference<CURRENT_VERSION>>(
        &self,
        hash_ref: &KeyPackageRef,
    ) -> Result<(), Self::Error> {
        self.remove(KEY_PACKAGE, &Global(hash_ref))
    }

    fn delete_psk<PskKey: traits::PskId<CURRENT_VERSION>>(
        &self,
        psk_id: &PskKey,
    ) -> Result<(), Self::Error> {
        self.remove(PSK, &Global(psk_id))
    }
}
