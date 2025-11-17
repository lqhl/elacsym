//! elacsym-api: REST API for elacsym vector database

pub mod handlers;
pub mod models;
pub mod server;
pub mod service;
pub mod service_s3;

pub use server::ApiServer;
pub use service::VectorDBService;
pub use service_s3::{S3VectorDBService, BuildIndexResponse};
