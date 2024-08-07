// crates
// internal

pub mod blob;

pub trait DaEncoder {
    type EncodedData;
    type Error;

    fn encode(&self, b: &[u8]) -> Result<Self::EncodedData, Self::Error>;
}

pub trait DaVerifier {
    type DaBlob;
    type Error;

    fn verify(&self, blob: &Self::DaBlob) -> Result<(), Self::Error>;
}

pub trait DaDispersal {
    type EncodedData;
    type Error;

    fn disperse(&self, encoded_data: Self::EncodedData) -> Result<(), Self::Error>;
}

pub trait Signer {
    fn sign(&self, message: &[u8]) -> Vec<u8>;
}
