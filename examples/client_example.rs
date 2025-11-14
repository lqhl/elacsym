//! Client example showing how to interact with elacsym API

use serde_json::json;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let base_url = "http://localhost:8080";

    println!("elacsym Client Example");
    println!("======================\n");

    // 1. Check health
    println!("1. Checking server health...");
    let health_response = client
        .get(&format!("{}/health", base_url))
        .send()
        .await?;
    println!("   Status: {}\n", health_response.status());

    // 2. Create namespace (optional, default namespace should exist)
    println!("2. Creating namespace 'test' with dimension 128...");
    let create_ns_response = client
        .post(&format!("{}/namespaces", base_url))
        .json(&json!({
            "name": "test",
            "dimension": 128
        }))
        .send()
        .await?;
    println!("   Status: {}\n", create_ns_response.status());

    // 3. Upsert vectors
    println!("3. Upserting 5 vectors...");
    let mut vectors = Vec::new();
    for i in 0..5 {
        let values: Vec<f32> = (0..128).map(|j| (i * 128 + j) as f32 / 1000.0).collect();
        vectors.push(json!({
            "id": format!("vec_{}", i),
            "values": values,
            "metadata": {
                "index": i,
                "category": format!("cat_{}", i % 3)
            }
        }));
    }

    let upsert_response = client
        .post(&format!("{}/vectors/upsert", base_url))
        .json(&json!({
            "vectors": vectors,
            "namespace": "test"
        }))
        .send()
        .await?;

    let upsert_result: serde_json::Value = upsert_response.json().await?;
    println!("   Result: {}\n", upsert_result);

    // Wait a bit for indexing
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // 4. Query for similar vectors
    println!("4. Querying for top 3 similar vectors...");
    let query_vector: Vec<f32> = (0..128).map(|i| i as f32 / 1000.0).collect();

    let query_response = client
        .post(&format!("{}/vectors/query", base_url))
        .json(&json!({
            "vector": query_vector,
            "top_k": 3,
            "namespace": "test",
            "include_values": false,
            "include_metadata": true
        }))
        .send()
        .await?;

    let query_result: serde_json::Value = query_response.json().await?;
    println!("   Matches:\n{}\n", serde_json::to_string_pretty(&query_result)?);

    // 5. Fetch specific vectors
    println!("5. Fetching vectors by ID...");
    let fetch_response = client
        .post(&format!("{}/vectors/fetch", base_url))
        .json(&json!({
            "ids": ["vec_0", "vec_2"],
            "namespace": "test"
        }))
        .send()
        .await?;

    let fetch_result: serde_json::Value = fetch_response.json().await?;
    println!("   Fetched vectors:\n{}\n", serde_json::to_string_pretty(&fetch_result)?);

    // 6. Delete vectors
    println!("6. Deleting vectors...");
    let delete_response = client
        .post(&format!("{}/vectors/delete", base_url))
        .json(&json!({
            "ids": ["vec_4"],
            "namespace": "test"
        }))
        .send()
        .await?;

    let delete_result: serde_json::Value = delete_response.json().await?;
    println!("   Result: {}\n", delete_result);

    println!("✓ All operations completed successfully!");

    Ok(())
}
