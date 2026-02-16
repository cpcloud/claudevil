use std::collections::HashSet;

use tree_sitter::{Language, Node, Parser};

use crate::config::Config;
use crate::error::{Error, Result};

/// A contiguous chunk of source code with metadata.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub content: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub start_line: usize, // 1-indexed
    pub end_line: usize,   // 1-indexed, inclusive
}

/// A loaded language grammar with its chunking configuration.
struct LoadedLanguage {
    language: Language,
    chunk_on: HashSet<String>,
}

/// Tree-sitter based chunker using natively compiled grammars.
pub struct TreeSitterChunker {
    languages: Vec<(String, LoadedLanguage)>,
}

impl TreeSitterChunker {
    /// Create a chunker with all configured languages loaded.
    pub fn new(config: &Config) -> Result<Self> {
        let mut languages = Vec::new();

        for (name, lang_config) in &config.lang {
            let language = native_language(&lang_config.grammar).ok_or_else(|| {
                Error::Config(format!(
                    "unknown grammar '{}' for language '{name}' -- \
                     only built-in grammars are supported: \
                     tree-sitter-go, tree-sitter-rust, tree-sitter-python",
                    lang_config.grammar
                ))
            })?;

            let chunk_on: HashSet<String> = lang_config
                .chunk_on
                .as_ref()
                .map(|v| v.iter().cloned().collect())
                .unwrap_or_default();

            languages.push((name.clone(), LoadedLanguage { language, chunk_on }));
        }

        Ok(Self { languages })
    }

    /// Chunk source code for a given language.
    pub fn chunk_file(&self, source: &str, lang_name: &str) -> Result<Vec<Chunk>> {
        let loaded = self
            .languages
            .iter()
            .find(|(name, _)| name == lang_name)
            .map(|(_, loaded)| loaded)
            .ok_or_else(|| {
                Error::TreeSitter(format!("no grammar loaded for language '{lang_name}'"))
            })?;

        let mut parser = Parser::new();
        parser
            .set_language(&loaded.language)
            .map_err(|e| Error::TreeSitter(format!("set_language failed: {e}")))?;

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| Error::TreeSitter("parsing returned no tree".to_string()))?;

        let source_bytes = source.as_bytes();
        let mut chunks = Vec::new();
        collect_chunks(
            tree.root_node(),
            source_bytes,
            &loaded.chunk_on,
            lang_name,
            &mut chunks,
        );

        Ok(chunks)
    }
}

/// Map a grammar name to its natively compiled Language.
fn native_language(grammar: &str) -> Option<Language> {
    match grammar {
        "tree-sitter-go" => Some(tree_sitter_go::LANGUAGE.into()),
        "tree-sitter-rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "tree-sitter-python" => Some(tree_sitter_python::LANGUAGE.into()),
        _ => None,
    }
}

/// Recursively walk the AST and extract chunks for matching node kinds.
fn collect_chunks(
    node: Node<'_>,
    source: &[u8],
    chunk_on: &HashSet<String>,
    lang_name: &str,
    chunks: &mut Vec<Chunk>,
) {
    if chunk_on.contains(node.kind()) {
        let content = node.utf8_text(source).unwrap_or("").to_string();

        // Prepend doc comments from preceding siblings
        let content = prepend_comments(node, source, &content);

        let symbol_name = extract_symbol_name(node, source, lang_name);
        let start_line = node.start_position().row + 1;
        let end_line = node.end_position().row + 1;

        chunks.push(Chunk {
            content,
            symbol_name,
            symbol_kind: Some(node.kind().to_string()),
            start_line,
            end_line,
        });
    }

    // Recurse into children (nested matches produce separate chunks)
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_chunks(child, source, chunk_on, lang_name, chunks);
    }
}

/// Collect comment text from preceding siblings and prepend to content.
fn prepend_comments(node: Node<'_>, source: &[u8], content: &str) -> String {
    let mut comments = Vec::new();
    let mut sibling = node.prev_sibling();
    while let Some(sib) = sibling {
        if sib.kind() == "comment" || sib.kind() == "line_comment" || sib.kind() == "block_comment"
        {
            if let Ok(text) = sib.utf8_text(source) {
                comments.push(text.to_string());
            }
            sibling = sib.prev_sibling();
        } else {
            break;
        }
    }

    if comments.is_empty() {
        return content.to_string();
    }

    comments.reverse();
    let mut result = comments.join("\n");
    result.push('\n');
    result.push_str(content);
    result
}

/// Extract a human-readable symbol name from an AST node.
fn extract_symbol_name(node: Node<'_>, source: &[u8], lang_name: &str) -> Option<String> {
    // Special case: Rust impl_item -- combine type and trait fields
    if lang_name == "rust" && node.kind() == "impl_item" {
        return extract_rust_impl_name(node, source);
    }

    // Special case: Python decorated_definition -- drill into the definition child
    if lang_name == "python"
        && node.kind() == "decorated_definition"
        && let Some(def) = node.child_by_field_name("definition")
    {
        return def
            .child_by_field_name("name")
            .and_then(|n| n.utf8_text(source).ok())
            .map(|s| s.to_string());
    }

    // General case: try the "name" field
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
}

/// Extract `Type` or `Trait for Type` from a Rust impl item.
fn extract_rust_impl_name(node: Node<'_>, source: &[u8]) -> Option<String> {
    let type_node = node.child_by_field_name("type")?;
    let type_name = type_node.utf8_text(source).ok()?;

    if let Some(trait_node) = node.child_by_field_name("trait") {
        let trait_name = trait_node.utf8_text(source).ok()?;
        Some(format!("{trait_name} for {type_name}"))
    } else {
        Some(type_name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunker(languages: &[&str]) -> TreeSitterChunker {
        let mut config = Config::load().unwrap();
        // Keep only requested languages to speed up tests
        config
            .lang
            .retain(|name, _| languages.contains(&name.as_str()));
        TreeSitterChunker::new(&config).unwrap()
    }

    // ---------------------------------------------------------------
    // Go
    // ---------------------------------------------------------------

    #[test]
    fn go_function_declaration() {
        let chunker = make_chunker(&["go"]);
        let source = r#"package main

func hello() {
	fmt.Println("hello")
}
"#;
        let chunks = chunker.chunk_file(source, "go").unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_name.as_deref() == Some("hello")
                    && c.symbol_kind.as_deref() == Some("function_declaration")),
            "expected function_declaration for hello, got: {chunks:?}"
        );
    }

    #[test]
    fn go_method_declaration() {
        let chunker = make_chunker(&["go"]);
        let source = r#"package main

type Server struct{}

func (s *Server) Start() error {
	return nil
}
"#;
        let chunks = chunker.chunk_file(source, "go").unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_name.as_deref() == Some("Start")
                    && c.symbol_kind.as_deref() == Some("method_declaration")),
            "expected method_declaration for Start, got: {chunks:?}"
        );
    }

    #[test]
    fn go_type_declaration() {
        let chunker = make_chunker(&["go"]);
        let source = r#"package main

type User struct {
	Name string
	Age  int
}
"#;
        let chunks = chunker.chunk_file(source, "go").unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_kind.as_deref() == Some("type_declaration")),
            "expected type_declaration, got: {chunks:?}"
        );
    }

    #[test]
    fn go_const_and_var() {
        let chunker = make_chunker(&["go"]);
        let source = r#"package main

const MaxRetries = 3

var DefaultTimeout = 30
"#;
        let chunks = chunker.chunk_file(source, "go").unwrap();
        let kinds: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_kind.as_deref())
            .collect();
        assert!(
            kinds.contains(&"const_declaration"),
            "missing const_declaration: {kinds:?}"
        );
        assert!(
            kinds.contains(&"var_declaration"),
            "missing var_declaration: {kinds:?}"
        );
    }

    #[test]
    fn go_doc_comments_attached() {
        let chunker = make_chunker(&["go"]);
        let source = r#"package main

// Hello prints a greeting.
func Hello() {
	fmt.Println("hello")
}
"#;
        let chunks = chunker.chunk_file(source, "go").unwrap();
        let hello = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("Hello"))
            .expect("should find Hello");
        assert!(
            hello.content.contains("Hello prints a greeting"),
            "doc comment should be attached: {:?}",
            hello.content
        );
    }

    #[test]
    fn go_line_numbers_are_1_indexed() {
        let chunker = make_chunker(&["go"]);
        let source = "package main\n\nfunc Hello() {\n\tfmt.Println(\"hello\")\n}\n";
        let chunks = chunker.chunk_file(source, "go").unwrap();
        let hello = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("Hello"))
            .expect("should find Hello");
        assert_eq!(hello.start_line, 3);
        assert_eq!(hello.end_line, 5);
    }

    #[test]
    fn go_empty_file() {
        let chunker = make_chunker(&["go"]);
        let chunks = chunker.chunk_file("", "go").unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn go_realistic_http_handler() {
        let chunker = make_chunker(&["go"]);
        let source = r#"package api

import (
	"encoding/json"
	"net/http"
)

// UserService handles user operations.
type UserService struct {
	db    *sql.DB
	cache *redis.Client
}

// NewUserService creates a new UserService.
func NewUserService(db *sql.DB) *UserService {
	return &UserService{db: db}
}

// GetUser returns a user by ID.
func (s *UserService) GetUser(w http.ResponseWriter, r *http.Request) {
	json.NewEncoder(w).Encode("user")
}
"#;
        let chunks = chunker.chunk_file(source, "go").unwrap();
        let names: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_name.as_deref())
            .collect();
        assert!(
            names.contains(&"NewUserService"),
            "missing NewUserService: {names:?}"
        );
        assert!(names.contains(&"GetUser"), "missing GetUser: {names:?}");
    }

    // ---------------------------------------------------------------
    // Rust
    // ---------------------------------------------------------------

    #[test]
    fn rust_function_item() {
        let chunker = make_chunker(&["rust"]);
        let source = r#"fn main() {
    println!("hello");
}
"#;
        let chunks = chunker.chunk_file(source, "rust").unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_name.as_deref() == Some("main")
                    && c.symbol_kind.as_deref() == Some("function_item")),
            "expected function_item for main, got: {chunks:?}"
        );
    }

    #[test]
    fn rust_struct_and_impl() {
        let chunker = make_chunker(&["rust"]);
        let source = r#"struct Point {
    x: f64,
    y: f64,
}

impl Point {
    fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    fn distance(&self) -> f64 {
        (self.x * self.x + self.y * self.y).sqrt()
    }
}
"#;
        let chunks = chunker.chunk_file(source, "rust").unwrap();
        let kinds: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_kind.as_deref())
            .collect();
        assert!(
            kinds.contains(&"struct_item"),
            "missing struct_item: {kinds:?}"
        );
        assert!(kinds.contains(&"impl_item"), "missing impl_item: {kinds:?}");
        // Methods inside impl are also extracted as function_item
        assert!(
            kinds.contains(&"function_item"),
            "missing function_item: {kinds:?}"
        );
    }

    #[test]
    fn rust_impl_name_extraction() {
        let chunker = make_chunker(&["rust"]);
        let source = r#"struct Foo;

impl Foo {
    fn bar(&self) {}
}
"#;
        let chunks = chunker.chunk_file(source, "rust").unwrap();
        let impl_chunk = chunks
            .iter()
            .find(|c| c.symbol_kind.as_deref() == Some("impl_item"))
            .expect("should find impl_item");
        assert_eq!(impl_chunk.symbol_name.as_deref(), Some("Foo"));
    }

    #[test]
    fn rust_trait_impl_name() {
        let chunker = make_chunker(&["rust"]);
        let source = r#"struct Foo;

impl Display for Foo {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Foo")
    }
}
"#;
        let chunks = chunker.chunk_file(source, "rust").unwrap();
        let impl_chunk = chunks
            .iter()
            .find(|c| c.symbol_kind.as_deref() == Some("impl_item"))
            .expect("should find impl_item");
        assert_eq!(impl_chunk.symbol_name.as_deref(), Some("Display for Foo"));
    }

    #[test]
    fn rust_enum_and_trait() {
        let chunker = make_chunker(&["rust"]);
        let source = r#"enum Color {
    Red,
    Green,
    Blue,
}

trait Drawable {
    fn draw(&self);
}
"#;
        let chunks = chunker.chunk_file(source, "rust").unwrap();
        let kinds: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_kind.as_deref())
            .collect();
        assert!(kinds.contains(&"enum_item"), "missing enum_item: {kinds:?}");
        assert!(
            kinds.contains(&"trait_item"),
            "missing trait_item: {kinds:?}"
        );
    }

    #[test]
    fn rust_doc_comments_attached() {
        let chunker = make_chunker(&["rust"]);
        let source = r#"/// Adds two numbers.
fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#;
        let chunks = chunker.chunk_file(source, "rust").unwrap();
        let add = chunks
            .iter()
            .find(|c| c.symbol_name.as_deref() == Some("add"))
            .expect("should find add");
        assert!(
            add.content.contains("Adds two numbers"),
            "doc comment should be attached: {:?}",
            add.content
        );
    }

    // ---------------------------------------------------------------
    // Python
    // ---------------------------------------------------------------

    #[test]
    fn python_function_definition() {
        let chunker = make_chunker(&["python"]);
        let source = r#"def hello():
    print("hello")
"#;
        let chunks = chunker.chunk_file(source, "python").unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_name.as_deref() == Some("hello")
                    && c.symbol_kind.as_deref() == Some("function_definition")),
            "expected function_definition for hello, got: {chunks:?}"
        );
    }

    #[test]
    fn python_class_definition() {
        let chunker = make_chunker(&["python"]);
        let source = r#"class User:
    def __init__(self, name):
        self.name = name

    def greet(self):
        return f"Hello, {self.name}"
"#;
        let chunks = chunker.chunk_file(source, "python").unwrap();
        let kinds: Vec<_> = chunks
            .iter()
            .filter_map(|c| c.symbol_kind.as_deref())
            .collect();
        assert!(
            kinds.contains(&"class_definition"),
            "missing class_definition: {kinds:?}"
        );
        // Methods inside classes are also extracted
        assert!(
            kinds.contains(&"function_definition"),
            "missing function_definition: {kinds:?}"
        );
    }

    #[test]
    fn python_decorated_function() {
        let chunker = make_chunker(&["python"]);
        let source = r#"@app.route("/users")
def list_users():
    return []
"#;
        let chunks = chunker.chunk_file(source, "python").unwrap();
        assert!(
            chunks
                .iter()
                .any(|c| c.symbol_kind.as_deref() == Some("decorated_definition")),
            "expected decorated_definition, got: {chunks:?}"
        );
        let decorated = chunks
            .iter()
            .find(|c| c.symbol_kind.as_deref() == Some("decorated_definition"))
            .unwrap();
        assert_eq!(
            decorated.symbol_name.as_deref(),
            Some("list_users"),
            "decorated_definition should extract inner function name"
        );
    }

    #[test]
    fn python_nested_class_methods() {
        let chunker = make_chunker(&["python"]);
        let source = r#"class Outer:
    class Inner:
        def method(self):
            pass
"#;
        let chunks = chunker.chunk_file(source, "python").unwrap();
        // Should get: Outer (class), Inner (class, nested), method (function)
        assert!(
            chunks.len() >= 3,
            "expected at least 3 chunks (Outer, Inner, method), got {}",
            chunks.len()
        );
    }

    // ---------------------------------------------------------------
    // Cross-cutting
    // ---------------------------------------------------------------

    #[test]
    fn unknown_language_returns_error() {
        let chunker = make_chunker(&["go"]);
        let result = chunker.chunk_file("console.log('hi')", "javascript");
        assert!(result.is_err());
    }
}
