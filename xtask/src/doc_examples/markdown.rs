//! Explicit fenced Rust examples from repository-maintained articles.

use std::collections::BTreeSet;

pub const MAX_ARTICLE_BYTES: usize = 128 * 1024;
pub const MAX_CODE_BYTES: usize = 64 * 1024;
pub const MAX_BLOCKS: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expectation {
    Compiles,
    CompileFail { codes: BTreeSet<String> },
    Ignore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub line: usize,
    pub code: String,
    pub expectation: Expectation,
    pub no_run: bool,
    pub items: bool,
    pub edition: Option<u16>,
}

fn annotation(info: &str, line: usize) -> Result<Option<Block>, String> {
    let mut parts = info.split(',').map(str::trim);
    if parts.next() != Some("rust") {
        if info.starts_with("rust ") || info.starts_with("rust\t") {
            return Err(format!("line {line}: Rust modifiers must be comma separated"));
        }
        return Ok(None);
    }
    let mut flags = BTreeSet::new();
    let mut codes = BTreeSet::new();
    let mut edition = None;
    for part in parts {
        if !flags.insert(part) {
            return Err(format!("line {line}: repeated Rust modifier {part:?}"));
        }
        match part {
            "compile_fail" | "no_run" | "ignore" | "items" => {}
            "edition2018" | "edition2021" | "edition2024" => {
                let value = part[7..].parse().map_err(|_| "invalid edition")?;
                if edition.replace(value).is_some() {
                    return Err(format!("line {line}: more than one edition"));
                }
            }
            _ if part.len() == 5
                && part.as_bytes()[0] == b'E'
                && part.as_bytes()[1..].iter().all(u8::is_ascii_digit) =>
            {
                codes.insert(part.to_owned());
            }
            _ => return Err(format!("line {line}: unsupported Rust modifier {part:?}")),
        }
    }
    let fails = flags.contains("compile_fail");
    let ignore = flags.contains("ignore");
    let no_run = flags.contains("no_run");
    if (!fails && !codes.is_empty()) || (fails && no_run) || (ignore && flags.len() != 1) {
        return Err(format!("line {line}: conflicting Rust modifiers"));
    }
    Ok(Some(Block {
        line,
        code: String::new(),
        expectation: if ignore {
            Expectation::Ignore
        } else if fails {
            Expectation::CompileFail { codes }
        } else {
            Expectation::Compiles
        },
        no_run,
        items: flags.contains("items"),
        edition,
    }))
}

/// A CommonMark-style fence at zero to three leading spaces. An enclosing
/// fence owns its contents, so a displayed Rust fence never becomes a program.
fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let marker = *rest.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let width = rest.bytes().take_while(|b| *b == marker).count();
    (width >= 3).then(|| (marker, width, rest[width..].trim()))
}

pub fn parse(article: &str) -> Result<Vec<Block>, String> {
    if article.len() > MAX_ARTICLE_BYTES {
        return Err("article exceeds the byte limit".into());
    }
    let mut blocks = Vec::new();
    let mut active: Option<(u8, usize, usize, Option<Block>)> = None;
    for (index, line) in article.lines().enumerate() {
        let number = index + 1;
        if let Some((marker, width, _, block)) = &mut active {
            let closes =
                fence(line).is_some_and(|(m, n, info)| m == *marker && n >= *width && info.is_empty());
            if closes {
                if let Some(block) = block.take() {
                    if block.code.trim().is_empty() && block.expectation != Expectation::Ignore {
                        return Err(format!("line {}: empty checked Rust example", block.line));
                    }
                    blocks.push(block);
                }
                active = None;
            } else if let Some(block) = block {
                if block.code.len().saturating_add(line.len()).saturating_add(1) > MAX_CODE_BYTES {
                    return Err(format!("line {}: Rust example exceeds the byte limit", block.line));
                }
                block.code.push_str(line);
                block.code.push('\n');
            }
            continue;
        }
        if let Some((marker, width, info)) = fence(line) {
            let block = annotation(info, number)?;
            if block.is_some() && blocks.len() >= MAX_BLOCKS {
                return Err("article exceeds the Rust block limit".into());
            }
            active = Some((marker, width, number, block));
        } else if let Some((_, _, info)) = fence(line.trim_start())
            && annotation(info, number)?.is_some()
        {
            return Err(format!("line {number}: Rust fence needs at most three leading spaces"));
        }
    }
    if let Some((_, _, line, _)) = active {
        return Err(format!("line {line}: unclosed code fence"));
    }
    Ok(blocks)
}

/// Examples are standalone programs. `items` explicitly appends an empty main
/// for library definitions/tests; statements are never guessed into a wrapper.
/// Rustdoc hidden/escaped lines preserve the source line count for diagnostics.
pub fn program(block: &Block) -> String {
    let mut output = String::new();
    for line in block.code.lines() {
        let indent = line.bytes().take_while(|b| *b == b' ' || *b == b'\t').count();
        let rest = &line[indent..];
        output.push_str(&line[..indent]);
        let visible = if rest == "#" {
            ""
        } else if let Some(visible) = rest.strip_prefix("# ") {
            visible
        } else if let Some(escaped) = rest.strip_prefix("##") {
            output.push('#');
            escaped
        } else {
            rest
        };
        output.push_str(visible);
        output.push('\n');
    }
    if block.items {
        output.push_str("\nfn main() {}\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standalone_no_run_and_multiple_required_error_codes_stay_distinct() {
        let blocks = parse("```rust,no_run,edition2024\nfn main() {}\n```\n~~~rust,compile_fail,E0502,E0382\nfn main() {}\n~~~").unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].line, 1);
        assert!(blocks[0].no_run);
        assert_eq!(blocks[0].edition, Some(2024));
        assert_eq!(blocks[1].line, 4);
        assert_eq!(
            blocks[1].expectation,
            Expectation::CompileFail { codes: ["E0382".into(), "E0502".into()].into() }
        );
    }

    #[test]
    fn a_compile_fail_without_codes_still_needs_a_real_compiler_verdict_later() {
        let block = parse("```rust,compile_fail\ncompile_error!(\"owned\");\n```\n").unwrap().remove(0);
        assert_eq!(block.expectation, Expectation::CompileFail { codes: BTreeSet::new() });
    }

    #[test]
    fn misspelled_or_conflicting_flags_do_not_silently_remove_checks() {
        for info in [
            "rust,complie_fail",
            "rust,E0502",
            "rust,compile_fail,no_run",
            "rust,ignore,items",
            "rust,Eabcd",
            "rust,",
            "rust,no_run,no_run",
            "rust,compile_fail,E0502,E0502",
            "rust,edition2018,edition2024",
            "rust no_run",
        ] {
            assert!(parse(&format!("```{info}\nfn main() {{}}\n```\n")).is_err(), "{info}");
        }
    }

    #[test]
    fn displayed_rust_fences_inside_other_fences_are_not_executed() {
        assert!(parse("````text\n```rust\nfn main() {}\n```\n````\n").unwrap().is_empty());
    }

    #[test]
    fn longer_and_tilde_fences_use_their_own_closing_marker() {
        let block =
            parse("~~~~rust\n// ``` and ~~~ are not closing lines\nfn main() {}\n~~~~~\n").unwrap().remove(0);
        assert!(block.code.starts_with("// ```"));
        assert!(block.code.ends_with("fn main() {}\n"));
    }

    #[test]
    fn every_unclosed_fence_is_an_error() {
        for text in [
            "```rust\nfn main() {}\n",
            "~~~text\ndisplayed code",
            "```rust\nfn main() {}\n~~~\n",
            "```rust\nfn main() {}\n```not a closing line\n",
        ] {
            assert!(parse(text).is_err());
        }
    }

    #[test]
    fn code_and_article_admission_limits_count_bytes() {
        assert!(parse(&"x".repeat(MAX_ARTICLE_BYTES + 1)).is_err());
        assert!(parse(&format!("```rust\n{}\n```", "é".repeat(MAX_CODE_BYTES / 2))).is_err());
        let accepted = format!("```rust\n{}\n```", "x".repeat(MAX_CODE_BYTES - 1));
        assert_eq!(parse(&accepted).unwrap()[0].code.len(), MAX_CODE_BYTES);
    }

    #[test]
    fn block_budget_includes_explicit_ignores() {
        let sample = "```rust,ignore\nignored\n```\n";
        assert_eq!(parse(&sample.repeat(MAX_BLOCKS)).unwrap().len(), MAX_BLOCKS);
        assert!(parse(&sample.repeat(MAX_BLOCKS + 1)).is_err());
    }

    #[test]
    fn hidden_lines_preserve_attributes_unicode_and_line_mapping() {
        let block = parse("```rust,items\n# use std::fmt;\n#\n#![allow(dead_code)]\n##[derive(Debug)]\nstruct Owned; // пример\n```\n").unwrap().remove(0);
        assert_eq!(
            program(&block),
            "use std::fmt;\n\n#![allow(dead_code)]\n#[derive(Debug)]\nstruct Owned; // пример\n\nfn main() {}\n"
        );
    }

    #[test]
    fn standalone_programs_never_gain_a_synthetic_main() {
        let block = parse("```rust\nfn main() { assert_eq!(2+2,4); }\n```\n").unwrap().remove(0);
        assert_eq!(program(&block), block.code);
    }

    #[test]
    fn crlf_and_allowed_fence_indentation_keep_the_opening_line() {
        let block = parse("owned\r\n   ```rust\r\nfn main() {}\r\n   ```\r\n").unwrap().remove(0);
        assert_eq!(block.line, 2);
        assert_eq!(block.code, "fn main() {}\n");
        assert!(parse("    ```rust\nfn main() {}\n    ```\n").is_err());
    }

    #[test]
    fn empty_checks_are_refused_while_ignored_blocks_stay_explicit() {
        assert!(parse("```rust\n```\n").is_err());
        assert_eq!(parse("```rust,ignore\n```\n").unwrap()[0].expectation, Expectation::Ignore);
    }
}
