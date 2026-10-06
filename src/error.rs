#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Fs(#[from] crate::fs::FsError),

    #[error("{0}")]
    Storage(#[from] crate::storage::StorageError),

    #[error("{0}")]
    Eyre(#[from] color_eyre::Report),
}
