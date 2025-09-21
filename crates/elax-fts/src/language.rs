use std::{fmt, str::FromStr};

use tantivy::tokenizer::{
    AsciiFoldingFilter, Language as TantivyLanguage, LowerCaser, RemoveLongFilter, SimpleTokenizer,
    Stemmer, StopWordFilter, TextAnalyzer,
};

use serde::{de::Error as SerdeDeError, Deserialize, Deserializer, Serialize, Serializer};
use serde_with::rust::double_option;

const DEFAULT_TOKEN_LENGTH_LIMIT: usize = 40;

/// Languages supported by elacsym's BM25 tokenizer presets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FtsLanguage {
    Arabic,
    Danish,
    Dutch,
    English,
    Finnish,
    French,
    German,
    Greek,
    Hungarian,
    Italian,
    Norwegian,
    Portuguese,
    Romanian,
    Russian,
    Spanish,
    Swedish,
    Tamil,
    Turkish,
}

impl FtsLanguage {
    /// Short, human-friendly identifier for the language (ISO-639-1 when available).
    pub fn code(self) -> &'static str {
        match self {
            FtsLanguage::Arabic => "ar",
            FtsLanguage::Danish => "da",
            FtsLanguage::Dutch => "nl",
            FtsLanguage::English => "en",
            FtsLanguage::Finnish => "fi",
            FtsLanguage::French => "fr",
            FtsLanguage::German => "de",
            FtsLanguage::Greek => "el",
            FtsLanguage::Hungarian => "hu",
            FtsLanguage::Italian => "it",
            FtsLanguage::Norwegian => "no",
            FtsLanguage::Portuguese => "pt",
            FtsLanguage::Romanian => "ro",
            FtsLanguage::Russian => "ru",
            FtsLanguage::Spanish => "es",
            FtsLanguage::Swedish => "sv",
            FtsLanguage::Tamil => "ta",
            FtsLanguage::Turkish => "tr",
        }
    }
}

impl fmt::Display for FtsLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseFtsLanguageError;

impl fmt::Display for ParseFtsLanguageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unsupported FTS language")
    }
}

impl std::error::Error for ParseFtsLanguageError {}

impl FromStr for FtsLanguage {
    type Err = ParseFtsLanguageError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase();
        let language = match normalized.as_str() {
            "ar" | "arabic" => FtsLanguage::Arabic,
            "da" | "danish" => FtsLanguage::Danish,
            "nl" | "dutch" => FtsLanguage::Dutch,
            "en" | "english" => FtsLanguage::English,
            "fi" | "finnish" => FtsLanguage::Finnish,
            "fr" | "french" => FtsLanguage::French,
            "de" | "german" => FtsLanguage::German,
            "el" | "greek" => FtsLanguage::Greek,
            "hu" | "hungarian" => FtsLanguage::Hungarian,
            "it" | "italian" => FtsLanguage::Italian,
            "no" | "norwegian" => FtsLanguage::Norwegian,
            "pt" | "portuguese" => FtsLanguage::Portuguese,
            "ro" | "romanian" => FtsLanguage::Romanian,
            "ru" | "russian" => FtsLanguage::Russian,
            "es" | "spanish" => FtsLanguage::Spanish,
            "sv" | "swedish" => FtsLanguage::Swedish,
            "ta" | "tamil" => FtsLanguage::Tamil,
            "tr" | "turkish" => FtsLanguage::Turkish,
            _ => return Err(ParseFtsLanguageError),
        };
        Ok(language)
    }
}

impl Serialize for FtsLanguage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.code())
    }
}

impl<'de> Deserialize<'de> for FtsLanguage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        FtsLanguage::from_str(&value)
            .map_err(|_| SerdeDeError::custom(format!("unsupported FTS language `{value}`")))
    }
}

impl From<FtsLanguage> for TantivyLanguage {
    fn from(value: FtsLanguage) -> Self {
        match value {
            FtsLanguage::Arabic => TantivyLanguage::Arabic,
            FtsLanguage::Danish => TantivyLanguage::Danish,
            FtsLanguage::Dutch => TantivyLanguage::Dutch,
            FtsLanguage::English => TantivyLanguage::English,
            FtsLanguage::Finnish => TantivyLanguage::Finnish,
            FtsLanguage::French => TantivyLanguage::French,
            FtsLanguage::German => TantivyLanguage::German,
            FtsLanguage::Greek => TantivyLanguage::Greek,
            FtsLanguage::Hungarian => TantivyLanguage::Hungarian,
            FtsLanguage::Italian => TantivyLanguage::Italian,
            FtsLanguage::Norwegian => TantivyLanguage::Norwegian,
            FtsLanguage::Portuguese => TantivyLanguage::Portuguese,
            FtsLanguage::Romanian => TantivyLanguage::Romanian,
            FtsLanguage::Russian => TantivyLanguage::Russian,
            FtsLanguage::Spanish => TantivyLanguage::Spanish,
            FtsLanguage::Swedish => TantivyLanguage::Swedish,
            FtsLanguage::Tamil => TantivyLanguage::Tamil,
            FtsLanguage::Turkish => TantivyLanguage::Turkish,
        }
    }
}

/// Tunables for constructing language-specific analyzers.
#[derive(Clone, Debug)]
pub struct LanguageOptions {
    pub(crate) stemming: bool,
    pub(crate) stop_words: bool,
    pub(crate) ascii_folding: bool,
    pub(crate) lower_case: bool,
    pub(crate) remove_long_limit: Option<usize>,
}

impl Default for LanguageOptions {
    fn default() -> Self {
        Self {
            stemming: true,
            stop_words: true,
            ascii_folding: true,
            lower_case: true,
            remove_long_limit: Some(DEFAULT_TOKEN_LENGTH_LIMIT),
        }
    }
}

impl LanguageOptions {
    /// Creates a new [`LanguageOptions`] instance with default normalization and scoring choices.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables or disables stemming for the language pack.
    pub fn with_stemming(mut self, enable: bool) -> Self {
        self.stemming = enable;
        self
    }

    /// Enables or disables stop-word removal for the language pack.
    pub fn with_stop_words(mut self, enable: bool) -> Self {
        self.stop_words = enable;
        self
    }

    /// Enables or disables ASCII folding of accentuated characters.
    pub fn with_ascii_folding(mut self, enable: bool) -> Self {
        self.ascii_folding = enable;
        self
    }

    /// Enables or disables lower casing of tokens.
    pub fn with_lower_case(mut self, enable: bool) -> Self {
        self.lower_case = enable;
        self
    }

    /// Sets the maximum token length accepted by the analyzer. `None` disables the filter.
    pub fn with_remove_long_limit(mut self, limit: Option<usize>) -> Self {
        self.remove_long_limit = limit;
        self
    }

    pub(crate) fn default_name(&self, language: FtsLanguage) -> String {
        let mut name = format!("{}_search", language.code());
        if !self.stemming {
            name.push_str("_nostem");
        }
        if !self.stop_words {
            name.push_str("_nostop");
        }
        if !self.ascii_folding {
            name.push_str("_nofold");
        }
        if !self.lower_case {
            name.push_str("_rawcase");
        }
        match self.remove_long_limit {
            Some(limit) if limit != DEFAULT_TOKEN_LENGTH_LIMIT => {
                fmt::Write::write_fmt(&mut name, format_args!("_len{limit}"))
                    .expect("writing to string cannot fail")
            }
            None => name.push_str("_nolen"),
            _ => {}
        }
        name
    }

    pub(crate) fn build_analyzer(&self, language: FtsLanguage) -> TextAnalyzer {
        let mut builder = TextAnalyzer::builder(SimpleTokenizer::default()).dynamic();
        if let Some(limit) = self.remove_long_limit {
            builder = builder.filter_dynamic(RemoveLongFilter::limit(limit));
        }
        if self.lower_case {
            builder = builder.filter_dynamic(LowerCaser);
        }
        if self.ascii_folding {
            builder = builder.filter_dynamic(AsciiFoldingFilter);
        }
        if self.stop_words {
            if let Some(filter) = StopWordFilter::new(language.into()) {
                builder = builder.filter_dynamic(filter);
            }
        }
        if self.stemming {
            builder = builder.filter_dynamic(Stemmer::new(language.into()));
        }

        builder.build()
    }
}

/// Declarative configuration for constructing [`LanguagePack`] instances.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LanguagePackConfig {
    pub language: FtsLanguage,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub stemming: Option<bool>,
    #[serde(default)]
    pub stop_words: Option<bool>,
    #[serde(default)]
    pub ascii_folding: Option<bool>,
    #[serde(default)]
    pub lower_case: Option<bool>,
    #[serde(
        default,
        with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub remove_long_limit: Option<Option<usize>>,
}

impl LanguagePackConfig {
    /// Converts the configuration into an executable [`LanguagePack`].
    pub fn into_pack(self) -> LanguagePack {
        let mut pack = LanguagePack::new(self.language);
        if let Some(name) = self.name {
            pack = pack.with_name(name);
        }
        if let Some(stemming) = self.stemming {
            pack = pack.with_stemming(stemming);
        }
        if let Some(stop_words) = self.stop_words {
            pack = pack.with_stop_words(stop_words);
        }
        if let Some(ascii_folding) = self.ascii_folding {
            pack = pack.with_ascii_folding(ascii_folding);
        }
        if let Some(lower_case) = self.lower_case {
            pack = pack.with_lower_case(lower_case);
        }
        if let Some(limit) = self.remove_long_limit {
            pack = pack.with_remove_long_limit(limit);
        }
        pack
    }
}

impl From<LanguagePackConfig> for LanguagePack {
    fn from(value: LanguagePackConfig) -> Self {
        value.into_pack()
    }
}

/// Helper for configuring and registering language-specific analyzers with [`SchemaConfig`].
#[derive(Clone, Debug)]
pub struct LanguagePack {
    name: String,
    explicit_name: bool,
    language: FtsLanguage,
    options: LanguageOptions,
}

impl LanguagePack {
    /// Creates a new language pack using [`LanguageOptions::default`].
    pub fn new(language: FtsLanguage) -> Self {
        let options = LanguageOptions::default();
        let name = options.default_name(language);
        Self {
            name,
            explicit_name: false,
            language,
            options,
        }
    }

    /// Overrides the identifier used when registering this analyzer.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self.explicit_name = true;
        self
    }

    /// Replaces the analyzer options wholesale.
    pub fn with_options(mut self, options: LanguageOptions) -> Self {
        self.options = options;
        if !self.explicit_name {
            self.name = self.options.default_name(self.language);
        }
        self
    }

    /// Enables or disables stemming for this language pack.
    pub fn with_stemming(mut self, enable: bool) -> Self {
        self.options.stemming = enable;
        if !self.explicit_name {
            self.name = self.options.default_name(self.language);
        }
        self
    }

    /// Enables or disables stop-word removal for this language pack.
    pub fn with_stop_words(mut self, enable: bool) -> Self {
        self.options.stop_words = enable;
        if !self.explicit_name {
            self.name = self.options.default_name(self.language);
        }
        self
    }

    /// Enables or disables ASCII folding for this language pack.
    pub fn with_ascii_folding(mut self, enable: bool) -> Self {
        self.options.ascii_folding = enable;
        if !self.explicit_name {
            self.name = self.options.default_name(self.language);
        }
        self
    }

    /// Enables or disables lowercase normalization for this language pack.
    pub fn with_lower_case(mut self, enable: bool) -> Self {
        self.options.lower_case = enable;
        if !self.explicit_name {
            self.name = self.options.default_name(self.language);
        }
        self
    }

    /// Adjusts the token length limit applied to incoming text.
    pub fn with_remove_long_limit(mut self, limit: Option<usize>) -> Self {
        self.options.remove_long_limit = limit;
        if !self.explicit_name {
            self.name = self.options.default_name(self.language);
        }
        self
    }

    /// Returns the tokenizer identifier registered by this pack.
    pub fn tokenizer_name(&self) -> &str {
        &self.name
    }

    /// Returns a reference to the underlying analyzer options.
    pub fn options(&self) -> &LanguageOptions {
        &self.options
    }

    pub(crate) fn into_named_analyzer(self) -> (String, TextAnalyzer) {
        let analyzer = self.options.build_analyzer(self.language);
        (self.name, analyzer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn collect_tokens(mut analyzer: TextAnalyzer, text: &str) -> Vec<String> {
        let mut stream = analyzer.token_stream(text);
        let mut tokens = Vec::new();
        while stream.advance() {
            tokens.push(stream.token().text.clone());
        }
        tokens
    }

    #[test]
    fn fts_language_parses_codes_and_names() {
        assert_eq!("en".parse::<FtsLanguage>().unwrap(), FtsLanguage::English);
        assert_eq!(
            "English".parse::<FtsLanguage>().unwrap(),
            FtsLanguage::English
        );
        assert_eq!(
            "FRENCH".parse::<FtsLanguage>().unwrap(),
            FtsLanguage::French
        );
        assert!(FtsLanguage::from_str("unknown").is_err());
    }

    #[test]
    fn language_pack_config_applies_overrides() {
        let config_json = json!({
            "language": "es",
            "name": "es_custom",
            "stemming": false,
            "stop_words": false,
            "ascii_folding": false,
            "lower_case": false,
            "remove_long_limit": null
        });

        let config: LanguagePackConfig = serde_json::from_value(config_json).unwrap();
        assert_eq!(config.remove_long_limit, Some(None));
        let pack: LanguagePack = config.into();
        assert_eq!(pack.tokenizer_name(), "es_custom");
        let options = pack.options();
        assert!(!options.stemming);
        assert!(!options.stop_words);
        assert!(!options.ascii_folding);
        assert!(!options.lower_case);
        assert_eq!(options.remove_long_limit, None);
    }

    #[test]
    fn english_defaults_stem_and_remove_stop_words() {
        let pack = LanguagePack::new(FtsLanguage::English);
        let analyzer = pack.options.build_analyzer(FtsLanguage::English);
        let tokens = collect_tokens(analyzer, "Running along the rivers in the forest");
        assert_eq!(tokens, vec!["run", "along", "river", "forest"]);
    }

    #[test]
    fn custom_pack_can_disable_normalization() {
        let pack = LanguagePack::new(FtsLanguage::Spanish)
            .with_stemming(false)
            .with_stop_words(false)
            .with_ascii_folding(false)
            .with_lower_case(false)
            .with_remove_long_limit(None);
        let analyzer = pack.options.build_analyzer(FtsLanguage::Spanish);
        let tokens = collect_tokens(analyzer, "Canción número TRECE");
        assert_eq!(tokens, vec!["Canción", "número", "TRECE"]);
    }

    #[test]
    fn languages_without_stop_words_still_register() {
        let pack = LanguagePack::new(FtsLanguage::Greek);
        let analyzer = pack.options.build_analyzer(FtsLanguage::Greek);
        let tokens = collect_tokens(analyzer, "Καλημέρα και καλή τύχη");
        // Stop words are not removed ("και" remains after stemming) and normalization still applies.
        assert!(tokens.contains(&"κα".to_string()));
        assert_eq!(tokens.first().map(String::as_str), Some("καλημερ"));
        assert_eq!(tokens.last().map(String::as_str), Some("τυχ"));
    }
}
