use thiserror::Error;

pub type Result<T, E = CatalogError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("this file is not a MediaNest catalog")]
    NotACatalog,
    #[error(
        "this catalog was written by a newer MediaNest (schema {found}, this version supports {supported})"
    )]
    TooNew { found: i32, supported: i32 },
    #[error("a catalog already exists here")]
    AlreadyExists,
    #[error("{0} not found")]
    NotFound(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("could not read or write file: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid metadata document: {0}")]
    Json(#[from] serde_json::Error),
}
