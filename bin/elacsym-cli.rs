//! elacsym CLI tool

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use reqwest::Client;
use serde_json::json;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "elacsym")]
#[command(about = "elacsym vector database CLI", long_about = None)]
struct Cli {
    /// Server URL
    #[arg(short, long, default_value = "http://localhost:8080")]
    url: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new namespace
    CreateNamespace {
        /// Namespace name
        name: String,

        /// Vector dimension
        #[arg(short, long)]
        dimension: usize,
    },

    /// Upsert vectors from a JSON file
    Upsert {
        /// Path to JSON file containing vectors
        file: PathBuf,

        /// Namespace (default: default)
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },

    /// Query for similar vectors
    Query {
        /// Path to JSON file containing query vector
        file: PathBuf,

        /// Number of results to return
        #[arg(short = 'k', long, default_value = "10")]
        top_k: usize,

        /// Namespace (default: default)
        #[arg(short, long, default_value = "default")]
        namespace: String,

        /// Include vector values in results
        #[arg(long)]
        include_values: bool,
    },

    /// Fetch vectors by ID
    Fetch {
        /// Vector IDs (comma-separated)
        ids: String,

        /// Namespace (default: default)
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },

    /// Delete vectors by ID
    Delete {
        /// Vector IDs (comma-separated)
        ids: String,

        /// Namespace (default: default)
        #[arg(short, long, default_value = "default")]
        namespace: String,
    },

    /// Check server health
    Health,

    /// Generate example configuration file
    GenConfig {
        /// Output path for config file
        #[arg(short, long, default_value = "config.toml")]
        output: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = Client::new();

    match cli.command {
        Commands::CreateNamespace { name, dimension } => {
            create_namespace(&client, &cli.url, &name, dimension).await?;
        }
        Commands::Upsert { file, namespace } => {
            upsert_vectors(&client, &cli.url, &file, &namespace).await?;
        }
        Commands::Query {
            file,
            top_k,
            namespace,
            include_values,
        } => {
            query_vectors(&client, &cli.url, &file, top_k, &namespace, include_values).await?;
        }
        Commands::Fetch { ids, namespace } => {
            fetch_vectors(&client, &cli.url, &ids, &namespace).await?;
        }
        Commands::Delete { ids, namespace } => {
            delete_vectors(&client, &cli.url, &ids, &namespace).await?;
        }
        Commands::Health => {
            check_health(&client, &cli.url).await?;
        }
        Commands::GenConfig { output } => {
            generate_config(&output)?;
        }
    }

    Ok(())
}

async fn create_namespace(client: &Client, url: &str, name: &str, dimension: usize) -> Result<()> {
    println!("Creating namespace '{}' with dimension {}...", name, dimension);

    let response = client
        .post(&format!("{}/namespaces", url))
        .json(&json!({
            "name": name,
            "dimension": dimension
        }))
        .send()
        .await
        .context("Failed to send request")?;

    if response.status().is_success() {
        println!("✓ Namespace created successfully");
    } else {
        let error = response.text().await?;
        println!("✗ Failed to create namespace: {}", error);
    }

    Ok(())
}

async fn upsert_vectors(client: &Client, url: &str, file: &PathBuf, namespace: &str) -> Result<()> {
    println!("Upserting vectors from {:?} to namespace '{}'...", file, namespace);

    let content = std::fs::read_to_string(file)
        .context("Failed to read input file")?;

    let vectors: serde_json::Value = serde_json::from_str(&content)
        .context("Failed to parse JSON")?;

    let response = client
        .post(&format!("{}/vectors/upsert", url))
        .json(&json!({
            "vectors": vectors,
            "namespace": namespace
        }))
        .send()
        .await
        .context("Failed to send request")?;

    let result: serde_json::Value = response.json().await?;
    println!("✓ Result: {}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

async fn query_vectors(
    client: &Client,
    url: &str,
    file: &PathBuf,
    top_k: usize,
    namespace: &str,
    include_values: bool,
) -> Result<()> {
    println!("Querying for top {} similar vectors...", top_k);

    let content = std::fs::read_to_string(file)
        .context("Failed to read query file")?;

    let query_vector: Vec<f32> = serde_json::from_str(&content)
        .context("Failed to parse query vector")?;

    let response = client
        .post(&format!("{}/vectors/query", url))
        .json(&json!({
            "vector": query_vector,
            "top_k": top_k,
            "namespace": namespace,
            "include_values": include_values,
            "include_metadata": true
        }))
        .send()
        .await
        .context("Failed to send request")?;

    let result: serde_json::Value = response.json().await?;
    println!("✓ Results:\n{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

async fn fetch_vectors(client: &Client, url: &str, ids: &str, namespace: &str) -> Result<()> {
    let id_list: Vec<String> = ids.split(',').map(|s| s.trim().to_string()).collect();

    println!("Fetching {} vectors from namespace '{}'...", id_list.len(), namespace);

    let response = client
        .post(&format!("{}/vectors/fetch", url))
        .json(&json!({
            "ids": id_list,
            "namespace": namespace
        }))
        .send()
        .await
        .context("Failed to send request")?;

    let result: serde_json::Value = response.json().await?;
    println!("✓ Results:\n{}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

async fn delete_vectors(client: &Client, url: &str, ids: &str, namespace: &str) -> Result<()> {
    let id_list: Vec<String> = ids.split(',').map(|s| s.trim().to_string()).collect();

    println!("Deleting {} vectors from namespace '{}'...", id_list.len(), namespace);

    let response = client
        .post(&format!("{}/vectors/delete", url))
        .json(&json!({
            "ids": id_list,
            "namespace": namespace
        }))
        .send()
        .await
        .context("Failed to send request")?;

    let result: serde_json::Value = response.json().await?;
    println!("✓ Result: {}", serde_json::to_string_pretty(&result)?);

    Ok(())
}

async fn check_health(client: &Client, url: &str) -> Result<()> {
    println!("Checking server health...");

    let response = client
        .get(&format!("{}/health", url))
        .send()
        .await
        .context("Failed to send request")?;

    if response.status().is_success() {
        println!("✓ Server is healthy");
    } else {
        println!("✗ Server is not healthy: {}", response.status());
    }

    Ok(())
}

fn generate_config(output: &PathBuf) -> Result<()> {
    use elacsym_config::Config;

    println!("Generating configuration file at {:?}...", output);

    let config = Config::default();
    config.to_file(output)
        .context("Failed to write config file")?;

    println!("✓ Configuration file generated successfully");
    println!("\nEdit the file and run:");
    println!("  elacsym-server {:?}", output);

    Ok(())
}
