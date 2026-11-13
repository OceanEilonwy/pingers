use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("a problem occurred with the AEAD")]
    Aead(#[from] aead::Error),
    #[error("a stream chunk was larger than the maximum allowed size")]
    ChunkTooLarge,
    /// Pingers patch: draft-ietf-ohai-chunked-ohttp-08 section 6.
    #[error("a non-final chunk had no data")]
    EmptyChunk,
    #[error("an error was found in the format")]
    Format,
    #[error("a problem occurred with HPKE: {0}")]
    Hpke(#[from] ::hpke::HpkeError),
    #[error("an internal error occurred")]
    Internal,
    #[error("the wrong type of key was provided for the selected KEM")]
    InvalidKeyType,
    #[error("the wrong KEM was specified")]
    InvalidKem,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("the key ID was invalid")]
    KeyId,
    #[error("the configuration contained too many symmetric suites")]
    TooManySymmetricSuites,
    #[error("a field was truncated")]
    Truncated,
    #[error("the configuration was not supported")]
    Unsupported,
}

impl From<std::num::TryFromIntError> for Error {
    fn from(_v: std::num::TryFromIntError) -> Self {
        Self::TooManySymmetricSuites
    }
}

// If this happens, it's a programming error in this crate.
impl From<std::array::TryFromSliceError> for Error {
    fn from(_v: std::array::TryFromSliceError) -> Self {
        Self::Internal
    }
}

pub type Res<T> = Result<T, Error>;
