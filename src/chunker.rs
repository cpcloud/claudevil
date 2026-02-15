use std::fmt;
use std::path::Path;

/// A contiguous chunk of source code with metadata.
#[derive(Debug, Clone)]
pub struct Chunk {
    pub content: String,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<SymbolKind>,
    pub package_name: Option<String>,
    pub start_line: usize, // 1-indexed
    pub end_line: usize,   // 1-indexed, inclusive
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Package,
    Import,
    Function,
    Method,
    Type,
    Interface,
    Const,
    Var,
}

impl fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Package => write!(f, "package"),
            Self::Import => write!(f, "import"),
            Self::Function => write!(f, "func"),
            Self::Method => write!(f, "method"),
            Self::Type => write!(f, "type"),
            Self::Interface => write!(f, "interface"),
            Self::Const => write!(f, "const"),
            Self::Var => write!(f, "var"),
        }
    }
}

/// Detect language from file extension. Returns None for unsupported languages.
pub fn detect_language(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "go" => Some("go"),
        _ => None,
    }
}

/// Chunk a file based on its detected language.
pub fn chunk_file(source: &str, language: &str) -> Vec<Chunk> {
    match language {
        "go" => chunk_go(source),
        _ => Vec::new(),
    }
}

/// Go-aware chunking: extracts top-level declarations as coherent chunks.
fn chunk_go(source: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = source.lines().collect();
    let mut chunks = Vec::new();
    let mut package_name: Option<String> = None;
    let mut i = 0;

    while i < lines.len() {
        // Skip blank lines
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }

        // Collect preceding comment block
        let comment_start = i;
        while i < lines.len() && is_comment_line(lines[i]) {
            i += 1;
        }

        if i >= lines.len() {
            break;
        }

        let decl_start = if i > comment_start { comment_start } else { i };

        let trimmed = lines[i].trim();

        if trimmed.starts_with("package ") {
            if let Some(name) = trimmed
                .strip_prefix("package ")
                .map(|s| s.trim().to_string())
            {
                package_name = Some(name.clone());
                chunks.push(Chunk {
                    content: join_lines(&lines, decl_start, i),
                    symbol_name: Some(name),
                    symbol_kind: Some(SymbolKind::Package),
                    package_name: package_name.clone(),
                    start_line: decl_start + 1,
                    end_line: i + 1,
                });
            }
            i += 1;
        } else if trimmed.starts_with("import") {
            let end = if trimmed.contains('(') {
                find_closing(&lines, i, '(', ')')
            } else {
                i
            };
            chunks.push(Chunk {
                content: join_lines(&lines, decl_start, end),
                symbol_name: None,
                symbol_kind: Some(SymbolKind::Import),
                package_name: package_name.clone(),
                start_line: decl_start + 1,
                end_line: end + 1,
            });
            i = end + 1;
        } else if trimmed.starts_with("func ") {
            let (name, kind) = parse_func_signature(trimmed);
            let end = if source_contains_brace(&lines, i) {
                find_closing(&lines, i, '{', '}')
            } else {
                // Function type or forward declaration — single line
                i
            };
            chunks.push(Chunk {
                content: join_lines(&lines, decl_start, end),
                symbol_name: Some(name),
                symbol_kind: Some(kind),
                package_name: package_name.clone(),
                start_line: decl_start + 1,
                end_line: end + 1,
            });
            i = end + 1;
        } else if trimmed.starts_with("type ") {
            let (name, kind) = parse_type_declaration(trimmed);
            let end = if source_contains_brace(&lines, i) {
                find_closing(&lines, i, '{', '}')
            } else {
                // Type alias — single line
                i
            };
            chunks.push(Chunk {
                content: join_lines(&lines, decl_start, end),
                symbol_name: Some(name),
                symbol_kind: Some(kind),
                package_name: package_name.clone(),
                start_line: decl_start + 1,
                end_line: end + 1,
            });
            i = end + 1;
        } else if trimmed.starts_with("const") || trimmed.starts_with("var") {
            let is_const = trimmed.starts_with("const");
            let kind = if is_const {
                SymbolKind::Const
            } else {
                SymbolKind::Var
            };
            let (name, end) = if trimmed.contains('(') {
                (None, find_closing(&lines, i, '(', ')'))
            } else {
                let prefix = if is_const { "const " } else { "var " };
                let after = trimmed.strip_prefix(prefix).unwrap_or("");
                let name = after.split_whitespace().next().unwrap_or("").to_string();
                (Some(name), i)
            };
            chunks.push(Chunk {
                content: join_lines(&lines, decl_start, end),
                symbol_name: name,
                symbol_kind: Some(kind),
                package_name: package_name.clone(),
                start_line: decl_start + 1,
                end_line: end + 1,
            });
            i = end + 1;
        } else {
            // Not a recognized top-level declaration; skip the line
            i += 1;
        }
    }

    chunks
}

fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with("* ")
}

fn join_lines(lines: &[&str], start: usize, end: usize) -> String {
    lines[start..=end.min(lines.len() - 1)].join("\n")
}

/// Check whether a brace appears on or after line `start` (within a few lines).
fn source_contains_brace(lines: &[&str], start: usize) -> bool {
    // Look at the declaration line and up to 5 continuation lines for an opening brace
    for (j, line) in lines
        .iter()
        .enumerate()
        .take((start + 6).min(lines.len()))
        .skip(start)
    {
        if line.contains('{') {
            return true;
        }
        if j > start && line.trim().is_empty() {
            break;
        }
    }
    false
}

/// Find the line where `close` balances `open`, starting from `start`.
fn find_closing(lines: &[&str], start: usize, open: char, close: char) -> usize {
    let mut depth: i32 = 0;
    for (j, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
        }
    }
    // Fallback: end of file
    lines.len().saturating_sub(1)
}

/// Parse `func Name(` or `func (r *Type) Name(`.
fn parse_func_signature(line: &str) -> (String, SymbolKind) {
    let after_func = line.trim().strip_prefix("func ").unwrap_or("").trim_start();

    if after_func.starts_with('(') {
        // Method: func (receiver) Name(
        if let Some(paren_end) = after_func.find(')') {
            let receiver = &after_func[1..paren_end];
            let receiver_type = receiver
                .split_whitespace()
                .last()
                .unwrap_or("")
                .trim_start_matches('*');

            let after_receiver = after_func[paren_end + 1..].trim();
            let name = after_receiver.split(['(', ' ']).next().unwrap_or("");

            (format!("{receiver_type}.{name}"), SymbolKind::Method)
        } else {
            ("unknown".to_string(), SymbolKind::Method)
        }
    } else {
        // Function: func Name(
        let name = after_func.split(['(', ' ', '[']).next().unwrap_or("");
        (name.to_string(), SymbolKind::Function)
    }
}

/// Parse `type Name struct {` or `type Name interface {`.
fn parse_type_declaration(line: &str) -> (String, SymbolKind) {
    let after_type = line.trim().strip_prefix("type ").unwrap_or("").trim();
    let name = after_type
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();

    let kind = if after_type.contains("interface") {
        SymbolKind::Interface
    } else {
        SymbolKind::Type
    };

    (name, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // ---------------------------------------------------------------
    // detect_language
    // ---------------------------------------------------------------

    #[test]
    fn detect_language_go() {
        assert_eq!(detect_language(Path::new("main.go")), Some("go"));
        assert_eq!(
            detect_language(Path::new("pkg/server/handler.go")),
            Some("go")
        );
    }

    #[test]
    fn detect_language_unsupported() {
        assert_eq!(detect_language(Path::new("main.rs")), None);
        assert_eq!(detect_language(Path::new("index.js")), None);
        assert_eq!(detect_language(Path::new("Makefile")), None);
        assert_eq!(detect_language(Path::new("README.md")), None);
    }

    #[test]
    fn detect_language_no_extension() {
        assert_eq!(detect_language(Path::new("Dockerfile")), None);
    }

    // ---------------------------------------------------------------
    // chunk_file dispatch
    // ---------------------------------------------------------------

    #[test]
    fn chunk_file_unknown_language_returns_empty() {
        let chunks = chunk_file("fn main() {}", "rust");
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_file_dispatches_to_go() {
        let chunks = chunk_file("package main\n", "go");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].symbol_kind, Some(SymbolKind::Package));
    }

    // ---------------------------------------------------------------
    // Real-world Go file with mixed declarations
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_realistic_http_handler() {
        let source = r#"package api

import (
	"encoding/json"
	"net/http"
)

// UserService handles user-related operations.
type UserService struct {
	db    *sql.DB
	cache *redis.Client
}

// NewUserService creates a new UserService.
func NewUserService(db *sql.DB, cache *redis.Client) *UserService {
	return &UserService{
		db:    db,
		cache: cache,
	}
}

// GetUser returns a user by ID.
func (s *UserService) GetUser(w http.ResponseWriter, r *http.Request) {
	id := r.URL.Query().Get("id")
	if id == "" {
		http.Error(w, "missing id", http.StatusBadRequest)
		return
	}

	user, err := s.db.Query("SELECT * FROM users WHERE id = ?", id)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	json.NewEncoder(w).Encode(user)
}

// DeleteUser removes a user by ID.
func (s *UserService) DeleteUser(w http.ResponseWriter, r *http.Request) {
	id := r.URL.Query().Get("id")
	_, err := s.db.Exec("DELETE FROM users WHERE id = ?", id)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}
"#;
        let chunks = chunk_file(source, "go");

        // Should extract: package, import, type, NewUserService, GetUser, DeleteUser
        assert_eq!(chunks.len(), 6);

        // Package
        assert_eq!(chunks[0].symbol_name.as_deref(), Some("api"));
        assert_eq!(chunks[0].symbol_kind, Some(SymbolKind::Package));
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 1);

        // Multi-line import block
        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Import));
        assert!(chunks[1].content.contains("encoding/json"));
        assert!(chunks[1].content.contains("net/http"));
        assert_eq!(chunks[1].start_line, 3);
        assert_eq!(chunks[1].end_line, 6);

        // Type with doc comment
        assert_eq!(chunks[2].symbol_name.as_deref(), Some("UserService"));
        assert_eq!(chunks[2].symbol_kind, Some(SymbolKind::Type));
        assert!(chunks[2].content.contains("UserService handles"));

        // Constructor function with doc comment
        assert_eq!(chunks[3].symbol_name.as_deref(), Some("NewUserService"));
        assert_eq!(chunks[3].symbol_kind, Some(SymbolKind::Function));
        assert!(chunks[3].content.contains("NewUserService creates"));

        // GetUser method with doc comment
        assert_eq!(
            chunks[4].symbol_name.as_deref(),
            Some("UserService.GetUser")
        );
        assert_eq!(chunks[4].symbol_kind, Some(SymbolKind::Method));
        assert!(chunks[4].content.contains("GetUser returns a user"));

        // DeleteUser method with doc comment
        assert_eq!(
            chunks[5].symbol_name.as_deref(),
            Some("UserService.DeleteUser")
        );
        assert_eq!(chunks[5].symbol_kind, Some(SymbolKind::Method));
        assert!(chunks[5].content.contains("DeleteUser removes"));
    }

    // ---------------------------------------------------------------
    // Interface declarations
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_interface() {
        let source = r#"package storage

// Store is the main storage interface.
type Store interface {
	Get(key string) ([]byte, error)
	Set(key string, value []byte) error
	Delete(key string) error
}

type ReadOnlyStore interface {
	Get(key string) ([]byte, error)
}
"#;
        let chunks = chunk_file(source, "go");

        // package + 2 interfaces
        assert_eq!(chunks.len(), 3);

        assert_eq!(chunks[1].symbol_name.as_deref(), Some("Store"));
        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Interface));
        assert!(chunks[1].content.contains("Store is the main"));
        assert!(chunks[1].content.contains("Delete(key string) error"));

        assert_eq!(chunks[2].symbol_name.as_deref(), Some("ReadOnlyStore"));
        assert_eq!(chunks[2].symbol_kind, Some(SymbolKind::Interface));
    }

    // ---------------------------------------------------------------
    // Nested braces in function bodies
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_nested_braces() {
        let source = r#"package main

func Process(items []string) map[string]int {
	result := make(map[string]int)
	for _, item := range items {
		if len(item) > 0 {
			for _, ch := range item {
				result[string(ch)]++
			}
		}
	}
	return result
}
"#;
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 2); // package + function

        let func_chunk = &chunks[1];
        assert_eq!(func_chunk.symbol_name.as_deref(), Some("Process"));
        // The whole function body should be captured, not cut off at the first }
        assert!(func_chunk.content.contains("return result"));
        assert!(func_chunk.content.contains("result[string(ch)]++"));
    }

    // ---------------------------------------------------------------
    // Const and var declarations
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_single_const() {
        let source = "package config\n\nconst MaxRetries = 3\n";
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 2);

        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Const));
        assert_eq!(chunks[1].symbol_name.as_deref(), Some("MaxRetries"));
        assert!(chunks[1].content.contains("MaxRetries = 3"));
    }

    #[test]
    fn chunk_go_grouped_const_with_iota() {
        let source = r#"package status

const (
	StatusPending = iota
	StatusRunning
	StatusDone
	StatusFailed
)
"#;
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 2);

        let const_chunk = &chunks[1];
        assert_eq!(const_chunk.symbol_kind, Some(SymbolKind::Const));
        assert!(const_chunk.content.contains("StatusPending"));
        assert!(const_chunk.content.contains("StatusFailed"));
    }

    #[test]
    fn chunk_go_var_declarations() {
        let source = r#"package globals

var DefaultTimeout = 30

var (
	ErrNotFound  = errors.New("not found")
	ErrForbidden = errors.New("forbidden")
)
"#;
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 3); // package + single var + grouped var

        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Var));
        assert_eq!(chunks[1].symbol_name.as_deref(), Some("DefaultTimeout"));

        assert_eq!(chunks[2].symbol_kind, Some(SymbolKind::Var));
        // Grouped var has no single name
        assert!(chunks[2].content.contains("ErrNotFound"));
        assert!(chunks[2].content.contains("ErrForbidden"));
    }

    // ---------------------------------------------------------------
    // Type aliases and definitions
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_type_alias() {
        let source = "package types\n\ntype UserID string\n\ntype Score float64\n";
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 3);

        assert_eq!(chunks[1].symbol_name.as_deref(), Some("UserID"));
        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Type));

        assert_eq!(chunks[2].symbol_name.as_deref(), Some("Score"));
        assert_eq!(chunks[2].symbol_kind, Some(SymbolKind::Type));
    }

    // ---------------------------------------------------------------
    // Line numbers
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_line_numbers_are_1_indexed() {
        let source = r#"package main

import "fmt"

func Hello() {
	fmt.Println("hello")
}
"#;
        let chunks = chunk_file(source, "go");

        // package on line 1
        assert_eq!(chunks[0].start_line, 1);
        assert_eq!(chunks[0].end_line, 1);

        // import on line 3
        assert_eq!(chunks[1].start_line, 3);
        assert_eq!(chunks[1].end_line, 3);

        // func on lines 5-7
        assert_eq!(chunks[2].start_line, 5);
        assert_eq!(chunks[2].end_line, 7);
    }

    // ---------------------------------------------------------------
    // Doc comments are attached to their declaration
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_doc_comments_attached() {
        let source = r#"package pkg

// Config holds application configuration.
// It is loaded from environment variables.
type Config struct {
	Port int
	Host string
}

// NewConfig creates a Config from the environment.
func NewConfig() *Config {
	return &Config{Port: 8080, Host: "localhost"}
}
"#;
        let chunks = chunk_file(source, "go");

        let type_chunk = &chunks[1];
        assert!(type_chunk.content.contains("Config holds application"));
        assert!(type_chunk.content.contains("loaded from environment"));
        assert!(type_chunk.content.contains("type Config struct"));

        let func_chunk = &chunks[2];
        assert!(func_chunk.content.contains("NewConfig creates"));
        assert!(func_chunk.content.contains("func NewConfig"));
    }

    // ---------------------------------------------------------------
    // Empty and minimal files
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_empty_file() {
        let chunks = chunk_file("", "go");
        assert!(chunks.is_empty());
    }

    #[test]
    fn chunk_go_package_only() {
        let chunks = chunk_file("package main\n", "go");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].symbol_kind, Some(SymbolKind::Package));
    }

    #[test]
    fn chunk_go_comments_only() {
        let source = "// This file is intentionally left blank.\n// Nothing here.\n";
        let chunks = chunk_file(source, "go");
        assert!(chunks.is_empty());
    }

    // ---------------------------------------------------------------
    // Single-line import
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_single_import() {
        let source = "package main\n\nimport \"fmt\"\n";
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1].symbol_kind, Some(SymbolKind::Import));
        assert!(chunks[1].content.contains("\"fmt\""));
    }

    // ---------------------------------------------------------------
    // Method with value receiver
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_value_receiver_method() {
        let source = r#"package point

type Point struct {
	X, Y float64
}

func (p Point) Distance() float64 {
	return math.Sqrt(p.X*p.X + p.Y*p.Y)
}
"#;
        let chunks = chunk_file(source, "go");
        let method = &chunks[2];
        assert_eq!(method.symbol_name.as_deref(), Some("Point.Distance"));
        assert_eq!(method.symbol_kind, Some(SymbolKind::Method));
    }

    // ---------------------------------------------------------------
    // Package name propagates to all chunks
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_package_name_propagated() {
        let source = r#"package mypackage

import "os"

func DoWork() error {
	return nil
}
"#;
        let chunks = chunk_file(source, "go");
        for chunk in &chunks {
            assert_eq!(
                chunk.package_name.as_deref(),
                Some("mypackage"),
                "chunk {:?} should have package_name=mypackage",
                chunk.symbol_kind
            );
        }
    }

    // ---------------------------------------------------------------
    // Multiple methods on the same type
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_multiple_methods_same_type() {
        let source = r#"package db

type Conn struct {
	url string
}

func (c *Conn) Open() error {
	return nil
}

func (c *Conn) Close() error {
	return nil
}

func (c *Conn) Ping() error {
	return nil
}
"#;
        let chunks = chunk_file(source, "go");

        let methods: Vec<_> = chunks
            .iter()
            .filter(|c| c.symbol_kind == Some(SymbolKind::Method))
            .collect();

        assert_eq!(methods.len(), 3);
        assert_eq!(methods[0].symbol_name.as_deref(), Some("Conn.Open"));
        assert_eq!(methods[1].symbol_name.as_deref(), Some("Conn.Close"));
        assert_eq!(methods[2].symbol_name.as_deref(), Some("Conn.Ping"));
    }

    // ---------------------------------------------------------------
    // Generic function (Go 1.18+)
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_generic_function() {
        let source = r#"package slices

func Map[T any, U any](s []T, f func(T) U) []U {
	result := make([]U, len(s))
	for i, v := range s {
		result[i] = f(v)
	}
	return result
}
"#;
        let chunks = chunk_file(source, "go");
        assert_eq!(chunks.len(), 2);

        let func_chunk = &chunks[1];
        assert_eq!(func_chunk.symbol_name.as_deref(), Some("Map"));
        assert_eq!(func_chunk.symbol_kind, Some(SymbolKind::Function));
        assert!(func_chunk.content.contains("return result"));
    }

    // ---------------------------------------------------------------
    // Struct with embedded fields and tags
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_struct_with_tags() {
        let source = r#"package model

type User struct {
	ID        int64  `json:"id" db:"id"`
	Name      string `json:"name" db:"name"`
	Email     string `json:"email" db:"email"`
	CreatedAt time.Time `json:"created_at" db:"created_at"`
	sync.Mutex
}
"#;
        let chunks = chunk_file(source, "go");
        let type_chunk = &chunks[1];
        assert_eq!(type_chunk.symbol_name.as_deref(), Some("User"));
        assert!(type_chunk.content.contains("sync.Mutex"));
        assert!(type_chunk.content.contains("`json:\"id\""));
    }

    // ---------------------------------------------------------------
    // Function with closure containing braces
    // ---------------------------------------------------------------

    #[test]
    fn chunk_go_function_with_closure() {
        let source = r#"package main

func RunServer() {
	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.Method == "POST" {
			w.WriteHeader(200)
		} else {
			w.WriteHeader(405)
		}
	})
	http.ListenAndServe(":8080", nil)
}
"#;
        let chunks = chunk_file(source, "go");
        let func_chunk = &chunks[1];
        assert_eq!(func_chunk.symbol_name.as_deref(), Some("RunServer"));
        // Must capture the entire function including the closure
        assert!(func_chunk.content.contains("ListenAndServe"));
    }
}
