use std::net::SocketAddr;

use anyhow::Result;
use elax_api::ApiServer;
use elax_store::LocalStore;

#[tokio::main]
async fn main() -> Result<()> {
    let data_root = std::env::var("ELAX_DATA_ROOT").unwrap_or_else(|_| ".elacsym".to_string());
    let addr: SocketAddr = std::env::var("ELAX_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()?;

    let store = LocalStore::new(data_root);
    let server = ApiServer::new(store);

    println!("query-node listening on {addr}");
    server.run(addr).await
}
