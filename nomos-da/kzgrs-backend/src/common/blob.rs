// std
// crates
use kzgrs::Proof;
use nomos_core::da::blob;
use serde::{Deserialize, Serialize};
use sha3::{Digest, Sha3_256};
// internal
use super::build_blob_id;
use super::ColumnIndex;
use crate::common::Column;
use crate::common::Commitment;
use crate::common::{
    deserialize_canonical, deserialize_vec_canonical, serialize_canonical, serialize_vec_canonical,
};

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaBlob {
    pub column: Column,
    pub column_idx: ColumnIndex,
    #[serde(
        serialize_with = "serialize_canonical",
        deserialize_with = "deserialize_canonical"
    )]
    pub column_commitment: Commitment,
    #[serde(
        serialize_with = "serialize_canonical",
        deserialize_with = "deserialize_canonical"
    )]
    pub aggregated_column_commitment: Commitment,
    #[serde(
        serialize_with = "serialize_canonical",
        deserialize_with = "deserialize_canonical"
    )]
    pub aggregated_column_proof: Proof,
    #[serde(
        serialize_with = "serialize_vec_canonical",
        deserialize_with = "deserialize_vec_canonical"
    )]
    pub rows_commitments: Vec<Commitment>,
    #[serde(
        serialize_with = "serialize_vec_canonical",
        deserialize_with = "deserialize_vec_canonical"
    )]
    pub rows_proofs: Vec<Proof>,
}

impl DaBlob {
    pub fn id(&self) -> Vec<u8> {
        build_blob_id(&self.aggregated_column_commitment, &self.rows_commitments).into()
    }

    pub fn column_id(&self) -> Vec<u8> {
        let mut hasher = Sha3_256::new();
        hasher.update(self.column.as_bytes());
        hasher.finalize().as_slice().to_vec()
    }

    pub fn column_len(&self) -> usize {
        self.column.as_bytes().len()
    }
}

impl blob::Blob for DaBlob {
    type BlobId = [u8; 32];
    type ColumnIndex = [u8; 2];

    fn id(&self) -> Self::BlobId {
        build_blob_id(&self.aggregated_column_commitment, &self.rows_commitments)
    }

    fn column_idx(&self) -> Self::ColumnIndex {
        self.column_idx.to_be_bytes()
    }
}
