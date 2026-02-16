use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{Error, Result};

/// Per-language configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct LangConfig {
    /// File extensions that map to this language (e.g. `["rs"]`).
    pub extensions: Vec<String>,
    /// Name of the tree-sitter grammar (e.g. `"tree-sitter-rust"`).
    pub grammar: String,
    /// AST node kinds to extract as chunks. If omitted, built-in defaults
    /// are used for known languages; unknown languages without this field
    /// cause an error at startup.
    pub chunk_on: Option<Vec<String>>,
}

/// Top-level configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub lang: HashMap<String, LangConfig>,
}

impl Config {
    /// Load configuration from the XDG config directory, merging with
    /// built-in defaults. If no config file exists, defaults are used as-is.
    pub fn load() -> Result<Self> {
        let mut config = Self::default_config();

        if let Some(path) = Self::config_path()
            && path.exists()
        {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| Error::Config(format!("failed to read {}: {e}", path.display())))?;
            let user: Config =
                toml::from_str(&raw).map_err(|e| Error::Config(format!("invalid config: {e}")))?;

            // User config overrides defaults per-language
            for (name, lang) in user.lang {
                config.lang.insert(name, lang);
            }

            tracing::info!("loaded config from {}", path.display());
        }

        // Validate: every language must have chunk_on resolved
        for (name, lang) in &mut config.lang {
            if lang.chunk_on.is_none() {
                let defaults = default_chunk_on(name);
                if defaults.is_empty() {
                    return Err(Error::Config(format!(
                        "language '{name}' has no chunk_on and no built-in defaults -- \
                         add chunk_on to the config to specify which AST node kinds to extract"
                    )));
                }
                lang.chunk_on = Some(defaults);
            }
        }

        Ok(config)
    }

    /// Map a file extension to its language name and config.
    pub fn language_for_extension(&self, ext: &str) -> Option<(&str, &LangConfig)> {
        self.lang
            .iter()
            .find(|(_, lang)| lang.extensions.iter().any(|e| e == ext))
            .map(|(name, lang)| (name.as_str(), lang))
    }

    /// Hardcoded defaults for Go, Rust, and Python.
    fn default_config() -> Self {
        let mut lang = HashMap::new();

        lang.insert(
            "go".to_string(),
            LangConfig {
                extensions: vec!["go".to_string()],
                grammar: "tree-sitter-go".to_string(),
                chunk_on: None, // resolved by default_chunk_on
            },
        );

        lang.insert(
            "rust".to_string(),
            LangConfig {
                extensions: vec!["rs".to_string()],
                grammar: "tree-sitter-rust".to_string(),
                chunk_on: None,
            },
        );

        lang.insert(
            "python".to_string(),
            LangConfig {
                extensions: vec!["py".to_string()],
                grammar: "tree-sitter-python".to_string(),
                chunk_on: None,
            },
        );

        Self { lang }
    }

    /// Path to the config file: `~/.config/claudevil/config.toml`
    fn config_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "claudevil")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Names of all configured languages, sorted for stable output.
    pub fn language_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.lang.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }
}

/// Built-in default `chunk_on` node kinds for known languages.
fn default_chunk_on(language: &str) -> Vec<String> {
    let kinds: &[&str] = match language {
        "go" => &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
            "const_declaration",
            "var_declaration",
        ],
        "rust" => &[
            "function_item",
            "impl_item",
            "struct_item",
            "enum_item",
            "trait_item",
            "mod_item",
            "const_item",
            "type_item",
            "static_item",
            "macro_definition",
        ],
        "python" => &[
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
        _ => &[],
    };
    kinds.iter().map(|s| (*s).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_three_languages() {
        let config = Config::default_config();
        assert_eq!(config.lang.len(), 3);
        assert!(config.lang.contains_key("go"));
        assert!(config.lang.contains_key("rust"));
        assert!(config.lang.contains_key("python"));
    }

    #[test]
    fn extension_mapping_go() {
        let mut config = Config::default_config();
        // Resolve chunk_on
        for (name, lang) in &mut config.lang {
            if lang.chunk_on.is_none() {
                lang.chunk_on = Some(default_chunk_on(name));
            }
        }

        let (name, lang) = config.language_for_extension("go").unwrap();
        assert_eq!(name, "go");
        assert_eq!(lang.grammar, "tree-sitter-go");
    }

    #[test]
    fn extension_mapping_rs() {
        let config = Config::default_config();
        let (name, lang) = config.language_for_extension("rs").unwrap();
        assert_eq!(name, "rust");
        assert_eq!(lang.grammar, "tree-sitter-rust");
    }

    #[test]
    fn extension_mapping_py() {
        let config = Config::default_config();
        let (name, lang) = config.language_for_extension("py").unwrap();
        assert_eq!(name, "python");
        assert_eq!(lang.grammar, "tree-sitter-python");
    }

    #[test]
    fn extension_mapping_unknown() {
        let config = Config::default_config();
        assert!(config.language_for_extension("js").is_none());
    }

    #[test]
    fn toml_parsing_with_chunk_on() {
        let raw = r#"
[lang.typescript]
extensions = ["ts", "tsx"]
grammar = "tree-sitter-typescript"
chunk_on = ["function_declaration", "class_declaration"]
"#;
        let config: Config = toml::from_str(raw).unwrap();
        let ts = &config.lang["typescript"];
        assert_eq!(ts.extensions, vec!["ts", "tsx"]);
        assert_eq!(ts.grammar, "tree-sitter-typescript");
        assert_eq!(
            ts.chunk_on.as_ref().unwrap(),
            &vec!["function_declaration", "class_declaration"]
        );
    }

    #[test]
    fn toml_parsing_without_chunk_on() {
        let raw = r#"
[lang.go]
extensions = ["go"]
grammar = "tree-sitter-go"
"#;
        let config: Config = toml::from_str(raw).unwrap();
        assert!(config.lang["go"].chunk_on.is_none());
    }

    #[test]
    fn default_chunk_on_go() {
        let kinds = default_chunk_on("go");
        assert!(kinds.contains(&"function_declaration".to_string()));
        assert!(kinds.contains(&"method_declaration".to_string()));
        assert!(kinds.contains(&"type_declaration".to_string()));
    }

    #[test]
    fn default_chunk_on_rust() {
        let kinds = default_chunk_on("rust");
        assert!(kinds.contains(&"function_item".to_string()));
        assert!(kinds.contains(&"impl_item".to_string()));
        assert!(kinds.contains(&"struct_item".to_string()));
    }

    #[test]
    fn default_chunk_on_python() {
        let kinds = default_chunk_on("python");
        assert!(kinds.contains(&"function_definition".to_string()));
        assert!(kinds.contains(&"class_definition".to_string()));
        assert!(kinds.contains(&"decorated_definition".to_string()));
    }

    #[test]
    fn default_chunk_on_unknown() {
        let kinds = default_chunk_on("haskell");
        assert!(kinds.is_empty());
    }

    #[test]
    fn language_names_sorted() {
        let config = Config::default_config();
        let names = config.language_names();
        assert_eq!(names, vec!["go", "python", "rust"]);
    }
}
