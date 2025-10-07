use elacsym::config::{
    AppConfig, DistributedRole, DistributedSection, S3StorageSection, StorageBackendKind,
    StorageSection,
};

#[test]
fn distributed_mode_requires_s3_backend() {
    let config = AppConfig {
        distributed: Some(DistributedSection {
            enabled: true,
            ..Default::default()
        }),
        ..Default::default()
    };

    let result = config.storage_runtime();
    assert!(
        result.is_err(),
        "Expected distributed + local storage to fail validation"
    );
}

#[test]
fn s3_backend_propagates_wal_prefix() {
    let config = AppConfig {
        storage: StorageSection {
            backend: StorageBackendKind::S3,
            local: None,
            s3: Some(S3StorageSection {
                bucket: "bucket".into(),
                region: "region".into(),
                endpoint: None,
                wal_prefix: Some(" tenant/prefix ".into()),
            }),
        },
        distributed: Some(DistributedSection {
            enabled: true,
            role: Some(DistributedRole::Indexer),
            ..Default::default()
        }),
        ..Default::default()
    };

    let (_storage, wal_config) = config
        .storage_runtime()
        .expect("S3 configuration should be valid");

    match wal_config {
        elacsym::namespace::WalConfig::S3 { prefix } => {
            assert_eq!(prefix.as_deref(), Some("tenant/prefix"));
        }
        other => panic!("Unexpected WAL config: {other:?}"),
    }
}
