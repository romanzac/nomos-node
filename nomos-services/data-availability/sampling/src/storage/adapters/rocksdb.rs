use kzgrs_backend::common::ColumnIndex;
use nomos_da_storage::{
    fs::load_blob,
    rocksdb::{key_bytes, DA_VERIFIED_KEY_PREFIX},
};
// std
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{marker::PhantomData, path::PathBuf};
// crates
use nomos_core::da::blob::Blob;
use nomos_storage::{
    backends::{rocksdb::RocksBackend, StorageSerde},
    StorageMsg, StorageService,
};
use overwatch_rs::{
    services::{relay::OutboundRelay, ServiceData},
    DynError,
};
// internal
use crate::storage::DaStorageAdapter;

pub struct RocksAdapter<B, S>
where
    S: StorageSerde + Send + Sync + 'static,
{
    settings: RocksAdapterSettings,
    storage_relay: OutboundRelay<StorageMsg<RocksBackend<S>>>,
    blob: PhantomData<B>,
}

#[async_trait::async_trait]
impl<B, S> DaStorageAdapter for RocksAdapter<B, S>
where
    S: StorageSerde + Send + Sync + 'static,
    B: Blob + DeserializeOwned + Clone + Send + Sync + 'static,
    B::BlobId: AsRef<[u8]> + Send,
{
    type Backend = RocksBackend<S>;
    type Blob = B;
    type Settings = RocksAdapterSettings;

    async fn new(
        settings: Self::Settings,
        storage_relay: OutboundRelay<<StorageService<Self::Backend> as ServiceData>::Message>,
    ) -> Self {
        Self {
            settings,
            storage_relay,
            blob: PhantomData,
        }
    }

    async fn get_blob(
        &self,
        blob_id: <Self::Blob as Blob>::BlobId,
        column_idx: ColumnIndex,
    ) -> Result<Option<Self::Blob>, DynError> {
        let column_idx = column_idx.to_be_bytes();
        let blob_idx = create_blob_idx(blob_id.as_ref(), &column_idx);

        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.storage_relay
            .send(StorageMsg::Load {
                key: key_bytes(DA_VERIFIED_KEY_PREFIX, blob_idx),
                reply_channel: reply_tx,
            })
            .await
            .expect("failed to send Load message to storage relay");

        if reply_rx.await?.is_some() {
            let blob_bytes = load_blob(
                self.settings.blob_storage_directory.clone(),
                blob_id.as_ref(),
                &column_idx,
            )
            .await?;
            Ok(S::deserialize(blob_bytes)
                .map(|blob| Some(blob))
                .unwrap_or_default())
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RocksAdapterSettings {
    pub blob_storage_directory: PathBuf,
}

// Combines a 32-byte blob ID (`[u8; 32]`) with a 2-byte column index
// (`u16` represented as `[u8; 2]`).
fn create_blob_idx(blob_id: &[u8], column_idx: &[u8]) -> [u8; 34] {
    let mut blob_idx = [0u8; 34];
    blob_idx[..blob_id.len()].copy_from_slice(blob_id);
    blob_idx[blob_id.len()..].copy_from_slice(column_idx);

    blob_idx
}
