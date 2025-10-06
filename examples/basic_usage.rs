//! Basic usage example for elacsym

use elacsym::namespace::Namespace;
use elacsym::storage::local::LocalStorage;
use elacsym::types::{
    AttributeSchema, AttributeType, AttributeValue, DistanceMetric, Document, FullTextConfig,
    Schema,
};
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::main]
async fn main() -> elacsym::Result<()> {
    println!("=== Elacsym Basic Usage Example ===\n");

    // 1. Setup storage (using local filesystem)
    let temp_dir = tempfile::tempdir().unwrap();
    println!("Using temporary storage at: {:?}", temp_dir.path());
    let storage = Arc::new(LocalStorage::new(temp_dir.path())?);

    // 2. Define schema
    let mut attributes = HashMap::new();
    attributes.insert(
        "title".to_string(),
        AttributeSchema {
            attr_type: AttributeType::String,
            indexed: false,
            full_text: FullTextConfig::Simple(true),
        },
    );
    attributes.insert(
        "category".to_string(),
        AttributeSchema {
            attr_type: AttributeType::String,
            indexed: true,
            full_text: FullTextConfig::Simple(false),
        },
    );
    attributes.insert(
        "score".to_string(),
        AttributeSchema {
            attr_type: AttributeType::Float,
            indexed: false,
            full_text: FullTextConfig::Simple(false),
        },
    );

    let schema = Schema {
        vector_dim: 128,
        vector_metric: DistanceMetric::L2,
        attributes,
    };

    // 3. Create namespace
    println!("Creating namespace 'my_docs'...");
    let namespace = Namespace::create("my_docs".to_string(), schema, storage.clone(), None, "main-node".to_string()).await?;
    println!("✓ Namespace created\n");

    // 4. Insert documents
    println!("Inserting documents...");
    let documents = vec![
        Document {
            id: 1,
            vector: Some(vec![1.0; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Rust Programming".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("tech".to_string()),
                );
                attrs.insert("score".to_string(), AttributeValue::Float(4.8));
                attrs
            },
        },
        Document {
            id: 2,
            vector: Some(vec![2.0; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Vector Database Guide".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("tech".to_string()),
                );
                attrs.insert("score".to_string(), AttributeValue::Float(4.5));
                attrs
            },
        },
        Document {
            id: 3,
            vector: Some(vec![3.0; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Machine Learning Basics".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("ai".to_string()),
                );
                attrs.insert("score".to_string(), AttributeValue::Float(4.7));
                attrs
            },
        },
        Document {
            id: 4,
            vector: Some(vec![1.5; 128]),
            attributes: {
                let mut attrs = HashMap::new();
                attrs.insert(
                    "title".to_string(),
                    AttributeValue::String("Rust for Beginners".to_string()),
                );
                attrs.insert(
                    "category".to_string(),
                    AttributeValue::String("tech".to_string()),
                );
                attrs.insert("score".to_string(), AttributeValue::Float(4.6));
                attrs
            },
        },
    ];

    let count = namespace.upsert(documents).await?;
    println!("✓ Inserted {} documents\n", count);

    // 5. Get namespace stats
    let stats = namespace.stats().await;
    println!("Namespace stats:");
    println!("  - Total documents: {}", stats.total_docs);
    println!("  - Total segments: {}", stats.segment_count);
    println!("  - Total size: {} bytes\n", stats.total_size_bytes);

    // 6. Query for similar vectors
    println!("Querying for vectors similar to [1.2, 1.2, ...]...");
    let query_vector = vec![1.2; 128];
    let results = namespace.query(Some(&query_vector), None, 3, None).await?;

    println!("Top 3 results:");
    for (i, (doc, distance)) in results.iter().enumerate() {
        println!(
            "  {}. Document ID: {}, Distance: {:.4}",
            i + 1,
            doc.id,
            distance
        );
    }
    println!();

    // 7. Query with different vector
    println!("Querying for vectors similar to [2.8, 2.8, ...]...");
    let query_vector2 = vec![2.8; 128];
    let results2 = namespace.query(Some(&query_vector2), None, 2, None).await?;

    println!("Top 2 results:");
    for (i, (doc, distance)) in results2.iter().enumerate() {
        println!(
            "  {}. Document ID: {}, Distance: {:.4}",
            i + 1,
            doc.id,
            distance
        );
    }

    println!("\n✓ Example completed successfully!");

    Ok(())
}
