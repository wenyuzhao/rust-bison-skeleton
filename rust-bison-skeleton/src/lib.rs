#![warn(missing_debug_implementations)]
#![warn(missing_docs)]
#![warn(trivial_casts, trivial_numeric_casts)]
#![warn(unused_qualifications)]
#![warn(deprecated_in_future)]
#![warn(unused_lifetimes)]
#![doc = include_str!("../README.md")]

use std::collections::HashSet;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::process::Command;

/// An error returned from `bison` executable
#[derive(Debug)]
pub struct BisonErr {
    /// stderr
    pub message: String,
    /// exit code
    pub code: Option<i32>,
}

impl fmt::Display for BisonErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BisonErr: {:#?} ({:#?})", self.message, self.code)
    }
}

impl Error for BisonErr {}

/// Creates a `.rs` file from the given `.y` file
/// Output file is created in the same directory
pub fn process_bison_file(filepath: &Path) -> Result<(), BisonErr> {
    let input = filepath;
    let output = filepath.with_extension("rs");

    let original_content = std::fs::read_to_string(input).ok();
    if let Some(ref content) = original_content {
        if let Some(augmented) = add_default_types_for_partial_typed_grammar(content) {
            let _ = std::fs::write(input, augmented);
        }
    }

    let bison_root_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("bison");
    let bison_root_file = bison_root_dir.join("main.m4");

    let args = &[
        "-Wno-other",
        "-S",
        bison_root_file.to_str().unwrap(),
        "-o",
        output.to_str().unwrap(),
        input.to_str().unwrap(),
    ];

    let cmd_output = Command::new("bison").args(args).output().unwrap();

    if let Some(content) = original_content {
        let _ = std::fs::write(input, content);
    }

    if cmd_output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8(cmd_output.stderr).unwrap();
        Err(BisonErr {
            message: stderr,
            code: cmd_output.status.code(),
        })
    }
}

fn add_default_types_for_partial_typed_grammar(content: &str) -> Option<String> {
    let sep_pos = content.find("\n%%\n")?;
    let header = &content[..sep_pos];
    let after_first_sep = &content[sep_pos + 4..];
    let rules = match after_first_sep.find("\n%%\n") {
        Some(pos) => &after_first_sep[..pos],
        None => after_first_sep,
    };

    let mut typed_nterms = HashSet::new();
    let mut has_any_type_decl = false;
    let mut value_type = "String".to_string();
    let mut in_type_decl = false;

    for line in header.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("%define") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix("api.value.type") {
                let rest = rest.trim_start();
                if let Some(inner) = rest.strip_prefix('{').and_then(|s| s.split('}').next()) {
                    value_type = inner.trim().to_string();
                }
            }
        }
        if let Some(after_pct_type) = trimmed.strip_prefix("%type") {
            let after_pct_type = after_pct_type.trim_start();
            if let Some(after_lt) = after_pct_type.strip_prefix('<') {
                let mut depth = 1usize;
                let mut gt_idx = None;
                for (i, ch) in after_lt.char_indices() {
                    if ch == '<' {
                        depth += 1;
                    } else if ch == '>' {
                        depth -= 1;
                        if depth == 0 {
                            gt_idx = Some(i);
                            break;
                        }
                    }
                }
                if let Some(end_idx) = gt_idx {
                    has_any_type_decl = true;
                    let syms_part = &after_lt[end_idx + 1..];
                    let syms_part = syms_part.split("/*").next().unwrap_or(syms_part);
                    let syms_part = syms_part.split("//").next().unwrap_or(syms_part);
                    for sym in syms_part.split_whitespace() {
                        typed_nterms.insert(sym.to_string());
                    }
                    in_type_decl = true;
                    continue;
                }
            }
        } else if in_type_decl {
            if !trimmed.is_empty()
                && !trimmed.starts_with('%')
                && !trimmed.starts_with("/*")
                && !trimmed.starts_with('*')
                && !trimmed.starts_with("//")
                && line.starts_with([' ', '\t'])
            {
                let syms_part = trimmed.split("/*").next().unwrap_or(trimmed);
                let syms_part = syms_part.split("//").next().unwrap_or(syms_part);
                for sym in syms_part.split_whitespace() {
                    typed_nterms.insert(sym.to_string());
                }
                continue;
            }
            in_type_decl = false;
        }
    }

    if !has_any_type_decl {
        return None;
    }

    let stripped_rules = strip_actions_and_comments(rules);
    let mut untyped_nterms = Vec::new();
    let mut seen_untyped = HashSet::new();
    for line in stripped_rules.lines() {
        let trimmed = line.trim_start();
        if let Some(colon_pos) = trimmed.find(':') {
            let candidate = trimmed[..colon_pos].trim_end();
            if !candidate.is_empty()
                && candidate
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !typed_nterms.contains(candidate)
                && seen_untyped.insert(candidate.to_string())
            {
                untyped_nterms.push(candidate.to_string());
            }
        }
    }

    let mut new_header_lines: Vec<String> = Vec::new();
    for line in header.lines() {
        let trimmed = line.trim_start();
        if let Some(after_tok) = trimmed.strip_prefix("%token") {
            let after_tok_trimmed = after_tok.trim_start();
            if !after_tok_trimmed.starts_with('<') {
                let leading_ws_len = line.len() - trimmed.len();
                let leading_ws = &line[..leading_ws_len];
                new_header_lines.push(format!("{leading_ws}%token <{value_type}>{after_tok}"));
                continue;
            }
        }
        new_header_lines.push(line.to_string());
    }

    if !untyped_nterms.is_empty() {
        let default_type_decl = format!(" %type <{value_type}> {}", untyped_nterms.join(" "));
        if let Some(last) = new_header_lines.last_mut() {
            last.push_str(&default_type_decl);
        } else {
            new_header_lines.push(default_type_decl);
        }
    }

    let mut out = new_header_lines.join("\n");
    out.push_str(&content[sep_pos..]);
    Some(out)
}

fn strip_actions_and_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    let mut depth = 0usize;

    while i < n {
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' && depth == 0 {
                    out.push('\n');
                }
                i += 1;
            }
            if i + 1 < n {
                i += 2;
            }
        } else if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i] == b'"' || bytes[i] == b'\'' {
            let quote = bytes[i];
            i += 1;
            while i < n && bytes[i] != quote {
                if bytes[i] == b'\\' {
                    i += 2;
                } else {
                    if bytes[i] == b'\n' && depth == 0 {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
            if i < n {
                i += 1;
            }
        } else if bytes[i] == b'{' {
            depth += 1;
            i += 1;
        } else if bytes[i] == b'}' {
            depth = depth.saturating_sub(1);
            i += 1;
        } else {
            if depth == 0 {
                out.push(bytes[i] as char);
            } else if bytes[i] == b'\n' {
                out.push('\n');
            }
            i += 1;
        }
    }
    out
}
