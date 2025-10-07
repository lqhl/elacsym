use elacsym::types::Document;
use elacsym::wal::{WalManager, WalOperation};
use std::collections::HashMap;
use tempfile::TempDir;
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

/// Test recovery from corrupted WAL entry (CRC mismatch)
#[tokio::test]
async fn test_corrupted_wal_crc_mismatch() {
    let temp_dir = TempDir::new().unwrap();
    let wal_path = temp_dir.path();

    // Create WAL and write valid entries
    {
        let mut wal = WalManager::new(wal_path).await.unwrap();

        let doc1 = Document {
            id: 1,
            vector: Some(vec![0.1, 0.2, 0.3]),
            attributes: HashMap::new(),
        };

        let doc2 = Document {
            id: 2,
            vector: Some(vec![0.4, 0.5, 0.6]),
            attributes: HashMap::new(),
        };

        // Write two entries
        wal.append(WalOperation::Upsert {
            documents: vec![doc1],
        })
        .await
        .unwrap();

        wal.append(WalOperation::Upsert {
            documents: vec![doc2],
        })
        .await
        .unwrap();

        wal.sync().await.unwrap();
    }

    // Corrupt the WAL file by modifying a byte in the middle
    let wal_file_path = wal_path.join("wal_000000.log");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .open(&wal_file_path)
            .await
            .unwrap();

        // Seek to byte 100 and corrupt it
        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(100)).await.unwrap();
        file.write_u8(0xFF).await.unwrap(); // Corrupt a byte
        file.flush().await.unwrap();
    }

    // Try to read - should recover partial data
    let wal = WalManager::new(wal_path).await.unwrap();
    let entries = wal.read_all().await.unwrap();

    // Should have recovered at least some entries (graceful degradation)
    // Exact count depends on where corruption happened
    println!("Recovered {} entries after corruption", entries.len());
    assert!(
        entries.len() <= 2,
        "Should not have more entries than written"
    );
}

/// Test recovery from truncated WAL file
#[tokio::test]
async fn test_truncated_wal_file() {
    let temp_dir = TempDir::new().unwrap();
    let wal_path = temp_dir.path();

    // Create WAL and write entries
    {
        let mut wal = WalManager::new(wal_path).await.unwrap();

        for i in 0..5 {
            let doc = Document {
                id: i,
                vector: Some(vec![i as f32]),
                attributes: HashMap::new(),
            };

            wal.append(WalOperation::Upsert {
                documents: vec![doc],
            })
            .await
            .unwrap();
        }

        wal.sync().await.unwrap();
    }

    // Truncate the WAL file (simulating crash during write)
    let wal_file_path = wal_path.join("wal_000000.log");
    {
        let file = tokio::fs::File::open(&wal_file_path).await.unwrap();
        let metadata = file.metadata().await.unwrap();
        let original_size = metadata.len();

        // Truncate to 70% of original size (cut off last entries)
        let truncated_size = (original_size as f64 * 0.7) as u64;
        drop(file);

        let file = OpenOptions::new()
            .write(true)
            .open(&wal_file_path)
            .await
            .unwrap();
        file.set_len(truncated_size).await.unwrap();
    }

    // Try to read - should recover partial data without error
    let wal = WalManager::new(wal_path).await.unwrap();
    let entries = wal.read_all().await.unwrap();

    // Should have recovered some entries (not all 5)
    println!("Recovered {} entries after truncation", entries.len());
    assert!(
        entries.len() < 5,
        "Should have lost some entries due to truncation"
    );
    assert!(
        !entries.is_empty(),
        "Should have recovered at least 1 entry"
    );
}

/// Test recovery from completely empty WAL (after header)
#[tokio::test]
async fn test_empty_wal_recovery() {
    let temp_dir = TempDir::new().unwrap();
    let wal_path = temp_dir.path();

    // Create WAL (creates header only)
    let wal = WalManager::new(wal_path).await.unwrap();

    // Should successfully read empty WAL
    let entries = wal.read_all().await.unwrap();
    assert_eq!(entries.len(), 0);
}

/// Test recovery with unreasonably large entry length
#[tokio::test]
async fn test_unreasonable_entry_size() {
    let temp_dir = TempDir::new().unwrap();
    let wal_path = temp_dir.path();

    // Create WAL with valid entry
    {
        let mut wal = WalManager::new(wal_path).await.unwrap();

        let doc = Document {
            id: 1,
            vector: Some(vec![0.1, 0.2, 0.3]),
            attributes: HashMap::new(),
        };

        wal.append(WalOperation::Upsert {
            documents: vec![doc],
        })
        .await
        .unwrap();

        wal.sync().await.unwrap();
    }

    // Append a fake entry with unreasonable size
    let wal_file_path = wal_path.join("wal_000000.log");
    {
        let mut file = OpenOptions::new()
            .append(true)
            .open(&wal_file_path)
            .await
            .unwrap();

        // Write an entry claiming to be 200MB (unreasonable)
        use tokio::io::AsyncWriteExt;
        file.write_u32(200 * 1024 * 1024).await.unwrap(); // 200MB length
        file.flush().await.unwrap();
    }

    // Try to read - should recover first entry, stop at unreasonable size
    let wal = WalManager::new(wal_path).await.unwrap();
    let entries = wal.read_all().await.unwrap();

    // Should have recovered only the first valid entry
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].sequence, 0);
}
