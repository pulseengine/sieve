//! A small Rust lexer: strips comments and collects string literal contents.
//! String and char literals stay intact in the output, so a `//` inside a
//! string is not a comment and a `'"'` char literal opens no string.

pub struct Lexed {
    /// The source without comments; string and char literals kept verbatim.
    pub code: String,
    /// The raw contents of every string literal (escapes not processed).
    pub literals: Vec<String>,
}

pub fn lex(src: &str) -> Lexed {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut lits = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if b[i..].starts_with(b"//") {
            i = match src[i..].find('\n') {
                Some(j) => i + j,
                None => n,
            };
        } else if b[i..].starts_with(b"/*") {
            let mut depth = 1;
            i += 2;
            while i < n && depth > 0 {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if c == b'r' && raw_open(b, i).is_some() && (i == 0 || !is_ident(b[i - 1])) {
            let hashes = raw_open(b, i).unwrap();
            let body = i + 2 + hashes;
            let mut close = String::from("\"");
            close.push_str(&"#".repeat(hashes));
            let end = match src[body..].find(&close) {
                Some(j) => body + j + close.len(),
                None => n,
            };
            let lit_end = end.saturating_sub(close.len()).max(body);
            lits.push(src[body..lit_end.min(n)].to_string());
            out.push_str(&src[i..end]);
            i = end;
        } else if c == b'"' {
            let mut j = i + 1;
            while j < n && b[j] != b'"' {
                j += if b[j] == b'\\' { 2 } else { 1 };
            }
            let j = j.min(n);
            lits.push(src[i + 1..j].to_string());
            out.push_str(&src[i..(j + 1).min(n)]);
            i = j + 1;
        } else if c == b'\'' {
            if let Some(len) = char_lit(b, i) {
                out.push_str(&src[i..i + len]);
                i += len;
            } else {
                out.push('\'');
                i += 1;
            }
        } else {
            // copy one UTF-8 character
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Lexed {
        code: out,
        literals: lits,
    }
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// At `r`, a raw string opener `r#*"`: the number of hashes.
fn raw_open(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() && b[j] == b'#' && j - i <= 255 {
        j += 1;
    }
    (j < b.len() && b[j] == b'"').then(|| j - i - 1)
}

/// A char literal `'x'` or `'\x'` at i: its byte length.
fn char_lit(b: &[u8], i: usize) -> Option<usize> {
    if i + 2 < b.len() && b[i + 1] != b'\\' && b[i + 1] != b'\'' && b[i + 2] == b'\'' {
        return Some(3);
    }
    if i + 3 < b.len() && b[i + 1] == b'\\' && b[i + 3] == b'\'' {
        return Some(4);
    }
    None
}

/// Process the escapes that matter for paths in a literal's raw contents.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments_go_strings_stay() {
        let l = lex("a // x \"no\"\n/* b /* c */ */ \"s // t\" '\"' r#\"q\"#");
        assert_eq!(l.literals, vec!["s // t", "q"]);
        assert!(!l.code.contains("no"));
        assert!(l.code.contains("\"s // t\""));
    }

    #[test]
    fn lifetimes_are_not_chars() {
        let l = lex("fn f<'a>(x: &'a str) { \"lit\" }");
        assert_eq!(l.literals, vec!["lit"]);
    }
}
