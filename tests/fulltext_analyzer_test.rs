use elacsym::index::fulltext::FullTextIndex;
use elacsym::types::FullTextConfig;

/// Test that stemming works correctly (e.g., "running" matches "run")
#[test]
fn test_analyzer_with_stemming() {
    let config = FullTextConfig::Advanced {
        language: "english".to_string(),
        stemming: true,
        remove_stopwords: false,
        case_sensitive: false,
        tokenizer: "default".to_string(),
    };

    let mut index = FullTextIndex::new_with_config("content".to_string(), config).unwrap();

    // Index document with "running"
    index.add(1, "The dog is running fast").unwrap();
    index.add(2, "The cat runs slowly").unwrap();

    // Search for "run" - should match both "running" and "runs" due to stemming
    let results = index.search("run", 10).unwrap();
    assert_eq!(results.len(), 2, "Stemming should match run/running/runs");
}

/// Test that stopword removal works (e.g., "the" is ignored)
#[test]
fn test_analyzer_with_stopwords() {
    let config = FullTextConfig::Advanced {
        language: "english".to_string(),
        stemming: false,
        remove_stopwords: true,
        case_sensitive: false,
        tokenizer: "default".to_string(),
    };

    let mut index = FullTextIndex::new_with_config("content".to_string(), config).unwrap();

    // Index documents
    index.add(1, "database technology").unwrap();
    index.add(2, "the database is great").unwrap();

    // Search for "database" - both should match even though doc 2 has "the"
    let results = index.search("database", 10).unwrap();
    assert_eq!(results.len(), 2);

    // Searching for just "the" should return no results (stopword removed)
    let results = index.search("the", 10).unwrap();
    assert_eq!(results.len(), 0, "Stopword 'the' should be removed");
}

/// Test case-sensitive search
#[test]
fn test_analyzer_case_sensitive() {
    let config = FullTextConfig::Advanced {
        language: "english".to_string(),
        stemming: false,
        remove_stopwords: false,
        case_sensitive: true,
        tokenizer: "default".to_string(),
    };

    let mut index = FullTextIndex::new_with_config("content".to_string(), config).unwrap();

    index.add(1, "Rust is great").unwrap();
    index.add(2, "rust programming").unwrap();

    // Case-sensitive: "Rust" should only match doc 1
    let results = index.search("Rust", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 1);

    // Case-sensitive: "rust" should only match doc 2
    let results = index.search("rust", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, 2);
}

/// Test case-insensitive search (default)
#[test]
fn test_analyzer_case_insensitive() {
    let config = FullTextConfig::Advanced {
        language: "english".to_string(),
        stemming: false,
        remove_stopwords: false,
        case_sensitive: false,
        tokenizer: "default".to_string(),
    };

    let mut index = FullTextIndex::new_with_config("content".to_string(), config).unwrap();

    index.add(1, "Rust is great").unwrap();
    index.add(2, "rust programming").unwrap();

    // Case-insensitive: "Rust" should match both
    let results = index.search("Rust", 10).unwrap();
    assert_eq!(results.len(), 2);

    // Case-insensitive: "rust" should also match both
    let results = index.search("rust", 10).unwrap();
    assert_eq!(results.len(), 2);
}

/// Test French language support
#[test]
fn test_analyzer_french_language() {
    let config = FullTextConfig::Advanced {
        language: "french".to_string(),
        stemming: true,
        remove_stopwords: true,
        case_sensitive: false,
        tokenizer: "default".to_string(),
    };

    let mut index = FullTextIndex::new_with_config("content".to_string(), config).unwrap();

    // French text
    index.add(1, "La base de données est excellente").unwrap();
    index.add(2, "Les bases de données").unwrap();

    // "base" should match both (with stemming)
    let results = index.search("base", 10).unwrap();
    assert_eq!(results.len(), 2);

    // French stopwords like "la" should be removed
    let results = index.search("la", 10).unwrap();
    assert_eq!(results.len(), 0, "French stopword 'la' should be removed");
}

/// Test Simple config (backward compatible)
#[test]
fn test_analyzer_simple_config() {
    let config = FullTextConfig::Simple(true);

    let mut index = FullTextIndex::new_with_config("content".to_string(), config).unwrap();

    // Should work with default settings (english, stemming, stopwords)
    index.add(1, "The dogs are running").unwrap();

    // Stemming should work
    let results = index.search("dog", 10).unwrap();
    assert_eq!(results.len(), 1);

    // Stopword should be removed
    let results = index.search("the", 10).unwrap();
    assert_eq!(results.len(), 0);
}
