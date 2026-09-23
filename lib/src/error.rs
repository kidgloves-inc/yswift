#[derive(Debug, thiserror::Error)]
pub enum CodingError {
    #[error("EncodingError")]
    EncodingError,
    #[error("DecodingError")]
    DecodingError,
    /// The update decoded, and integrating it into the document failed —
    /// yrs's `UpdateError`, e.g. a block whose parent is not a shared type.
    #[error("ApplyError")]
    ApplyError,
}

#[derive(Debug, thiserror::Error)]
pub enum YrsDocError {
    /// `YrsDoc::with_client_id` was given an id at or above 2^53, the width
    /// a yjs-compatible client id has.
    #[error("ClientIdOutOfRange")]
    ClientIdOutOfRange,
}
