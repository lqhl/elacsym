# BM25 Language Packs

This runbook documents the multi-language full-text search presets exposed by the `elax-fts` crate. Use these helpers to align Tantivy analyzers with product requirements while keeping tokenizer/stemming behavior consistent across namespaces.

## Defaults

Each [`LanguagePack`](../../crates/elax-fts/src/language.rs) starts from a balanced pipeline tuned for BM25 relevance:

- `SimpleTokenizer` tokenizes on whitespace/punctuation.
- `RemoveLongFilter::limit(40)` drops extremely long tokens that skew scoring.
- `LowerCaser` normalizes to lowercase.
- `AsciiFoldingFilter` strips accents (é → e) so ASCII-only queries still hit localized content.
- `StopWordFilter` removes language-specific filler words when available.
- `Stemmer` reduces inflected words to their roots (English "running" → "run").

The resulting tokenizer name defaults to `"<lang>_search"` (for example `"en_search"`), making it easy to reuse across multiple fields.

## Supported Languages

| Language   | ISO Code | Stop Words | Notes |
|------------|----------|------------|-------|
| Arabic     | ar       | No         | Stemming only; retain common particles. |
| Danish     | da       | Yes        | Uses Tantivy's Danish stop list and stemmer. |
| Dutch      | nl       | Yes        | |
| English    | en       | Yes        | Matches Tantivy's `en_stem` pipeline with ASCII folding. |
| Finnish    | fi       | Yes        | |
| French     | fr       | Yes        | Accents folded to ASCII before stemming. |
| German     | de       | Yes        | |
| Greek      | el       | No         | Stemming enabled; stop-word removal unsupported upstream. |
| Hungarian  | hu       | Yes        | |
| Italian    | it       | Yes        | |
| Norwegian  | no       | Yes        | |
| Portuguese | pt       | Yes        | |
| Romanian   | ro       | No         | |
| Russian    | ru       | Yes        | |
| Spanish    | es       | Yes        | |
| Swedish    | sv       | Yes        | |
| Tamil      | ta       | No         | |
| Turkish    | tr       | No         | |

When stop words are unavailable, enabling the option is a no-op; the analyzer still performs case folding, ASCII folding, and stemming.

## Usage Example

```rust
use elax_fts::{FtsLanguage, LanguagePack, SchemaConfig, TextFieldConfig};

let english = LanguagePack::new(FtsLanguage::English);
let spanish = LanguagePack::new(FtsLanguage::Spanish)
    .with_stop_words(false) // keep short product names such as "El"
    .with_name("es_catalog");

let schema = SchemaConfig::new("doc_id")
    .register_language_pack(english.clone())
    .register_language_pack(spanish.clone())
    .add_text_field(TextFieldConfig::new("title").stored().with_language(&english))
    .add_text_field(TextFieldConfig::new("description").with_language(&spanish));
```

Registering a pack multiple times is idempotent: if a tokenizer name already exists in the configuration it will not be re-added.

### Declarative Configuration

`LanguagePackConfig` enables storing analyzer choices in JSON/TOML configs and loading them at runtime. Languages accept either their ISO code (`"fr"`) or english name (`"french"`). Missing options inherit the defaults listed above.

```json
{
  "language": "fr",
  "name": "fr_blog",
  "stemming": true,
  "stop_words": false,
  "ascii_folding": false,
  "lower_case": true,
  "remove_long_limit": 64
}
```

In configuration files, setting `"remove_long_limit": null` disables the filter entirely.

## Customizing Options

`LanguagePack` exposes fluent helpers for common adjustments:

- `.with_stemming(false)` — disable stemming for exact-match scenarios.
- `.with_stop_words(false)` — retain filler words (useful for phrase queries).
- `.with_ascii_folding(false)` — keep accent information for locale-sensitive ranking.
- `.with_lower_case(false)` — preserve case-sensitive tokens (e.g., SKU identifiers).
- `.with_remove_long_limit(None)` — index every token regardless of length.

Advanced pipelines can be constructed via [`LanguageOptions`](../../crates/elax-fts/src/language.rs), then applied with `.with_options(options)` followed by `.with_name("custom")` to guarantee stable tokenizer identifiers.

## Operational Notes

- **Verification**: build a small Tantivy index in a scratch binary or unit test and inspect `token_stream` output to confirm stemming/stop words before large backfills.
- **Backfills**: ensure the same language packs are registered on indexers and query nodes before reprocessing documents; tokenizer mismatches cause BM25 scoring drift.
- **Mixed-language fields**: prefer per-language fields where possible. If a field mixes languages, choose the pack that best reflects the dominant audience and disable stemming to reduce false positives.

Keep this document current as new languages or token filters are added to Tantivy.
