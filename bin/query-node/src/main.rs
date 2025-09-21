use std::net::SocketAddr;

use anyhow::{Context, Result};
use elax_api::ApiServer;
use elax_config::ServiceConfig;
use elax_store::LocalStore;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path =
        std::env::var("ELAX_CONFIG").unwrap_or_else(|_| "configs/query-node.toml".to_string());
    let config = ServiceConfig::load(&config_path)
        .with_context(|| format!("loading config from {config_path}"))?;

    let bind_addr = std::env::var("ELAX_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());
    let addr: SocketAddr = bind_addr
        .parse()
        .with_context(|| format!("parsing ELAX_BIND '{bind_addr}'"))?;

    let mut store = LocalStore::new(&config.data_root);
    let object_store = config
        .object_store
        .build()
        .context("initializing object-store backend")?;
    store = store.with_object_store(object_store.store, object_store.prefix);

    let server = ApiServer::new(store);

    println!("query-node listening on {addr}");
    server.run(addr).await
}
