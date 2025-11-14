//! elacsym-api: REST API for elacsym vector database

pub mod handlers;
pub mod models;
pub mod server;
pub mod service;

pub use server::ApiServer;
pub use service::VectorDBService;
