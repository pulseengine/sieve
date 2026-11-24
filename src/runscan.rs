//! sieve#41: whether a `run:` script may write at, under or above a checked-out
//! path `p` (sieve#36). Before, any file or git command anywhere in the script
//! plus the bare word `p` anywhere (a comment, an issue reference, a read, a
//! branch name) counted, so every job that builds from its own checkout
//! refused. Now the script is split into simple commands, comments are
//! dropped, and a rewrite needs one command that holds a writer (a file or
//! git command, an in-place edit, a redirection; any word of the command, so
//! wrappers like `sudo -u x` and function bodies count) and names `p` as a
//! path: literally, through a variable assigned a path naming it, or by
//! running in it. What cannot be placed counts (fail closed): an unassigned
//! variable, an indirect expansion or an expression in a written path, a `cd`
//! to one, a command name held in an unknown variable, code piped into a
//! shell, `xargs` or `parallel` feeding a writer, inline interpreter code
//! naming `p`, and an unparseable script (an unclosed quote, substitution or
//! here-document).
//!
//! Not modelled (limits): a program or script that writes into `p` on its own
//! (`make`, `cargo`, `west`, `./setup.sh`, `source x.sh`), a write at the
//! workspace root without naming `p` (`git clean -fdx`, `rm -rf .`). A source
//! path counts like a destination (`cp gale/x /tmp` refuses).

use regex::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

const ROOT: char = '\u{0}';
const OUTSIDE: char = '\u{1}';
const UNKNOWN: char = '\u{2}';
const OPAQUE: char = '\u{3}';
const TAINT: char = '\u{4}';

#[derive(Default, Debug)]
struct Cmd {
    words: Vec<String>,
    redirects: Vec<String>,
    heredocs: Vec<String>,
    herestrings: Vec<String>,
    /// stdin comes from a pipe
    piped_in: bool,
    /// here-documents opened on this command (their bodies follow the line)
    want_heredocs: usize,
    /// an input process substitution `<(...)` (its output may be code)
    procsub_in: bool,
}

/// The text of a balanced `(...)` starting after the opening paren at `i`;
/// returns the inner text and the index of the closing paren.
fn balanced(c: &[char], mut i: usize) -> Option<(String, usize)> {
    let start = i;
    let mut depth = 1;
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '\'' => {
                i += 1;
                while i < c.len() && c[i] != '\'' {
                    i += 1;
                }
                if i >= c.len() {
                    return None;
                }
            }
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i >= c.len() {
                    return None;
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((c[start..i].iter().collect(), i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// `$((...))` arithmetic: the inner text of a `$(` substitution that is
/// itself one parenthesised group.
fn is_arith(text: &str) -> bool {
    // no trimming: `$( (cd x; ...) )` is a substitution of a subshell
    let c: Vec<char> = text.chars().collect();
    c.first() == Some(&'(')
        && c.last() == Some(&')')
        && balanced(&c, 1).is_some_and(|(_, end)| end == c.len() - 1)
}

/// The command substitutions inside arithmetic text (`$(( $(cmd) + 1 ))`),
/// to be parsed as scripts; an unclosed one yields an unparseable script.
fn arith_subs(text: &str) -> Vec<String> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '$' && c.get(i + 1) == Some(&'(') {
            match balanced(&c, i + 2) {
                Some((t, end)) => {
                    if is_arith(&t) {
                        out.extend(arith_subs(&t));
                    } else {
                        out.push(t);
                    }
                    i = end + 1;
                    continue;
                }
                None => {
                    out.push("'".into());
                    break;
                }
            }
        }
        if c[i] == '`' {
            match (i + 1..c.len()).find(|&k| c[k] == '`') {
                Some(end) => {
                    out.push(c[i + 1..end].iter().collect());
                    i = end + 1;
                    continue;
                }
                None => {
                    out.push("'".into());
                    break;
                }
            }
        }
        i += 1;
    }
    out
}

/// Split a script into simple commands. `None` when a quote, a substitution,
/// a parameter expansion or a here-document is not closed.
#[allow(unused_assignments)] // the macros reset state after the last word
fn parse(script: &str, depth: usize) -> Option<Vec<Cmd>> {
    if depth > 8 {
        return None;
    }
    let c: Vec<char> = script.chars().collect();
    let mut cmds: Vec<Cmd> = Vec::new();
    let mut inner: Vec<String> = Vec::new();
    let mut cur = Cmd::default();
    let mut word = String::new();
    let mut in_word = false;
    // what the next finished word is: a redirection target, an input file
    // (dropped), a here-string
    #[derive(PartialEq, Clone, Copy)]
    enum Next {
        Word,
        Redirect,
        Drop,
        HereString,
    }
    let mut next_kind = Next::Word;
    let mut heredocs: Vec<String> = Vec::new();
    // the command each pending here-document belongs to
    let mut hd_targets: Vec<usize> = Vec::new();
    let mut piped = false;
    let mut i = 0;
    macro_rules! fw {
        () => {
            if in_word {
                let w = std::mem::take(&mut word);
                match next_kind {
                    Next::Redirect => cur.redirects.push(w),
                    Next::HereString => cur.herestrings.push(w),
                    Next::Drop => {}
                    Next::Word => cur.words.push(w),
                }
                next_kind = Next::Word;
                in_word = false;
            }
        };
    }
    macro_rules! fc {
        ($pipe:expr) => {{
            fw!();
            if !cur.words.is_empty()
                || !cur.redirects.is_empty()
                || !cur.herestrings.is_empty()
                || cur.want_heredocs > 0
            {
                cur.piped_in = piped;
                for _ in 0..cur.want_heredocs {
                    hd_targets.push(cmds.len());
                }
                cmds.push(std::mem::take(&mut cur));
            }
            piped = $pipe;
        }};
    }
    while i < c.len() {
        let ch = c[i];
        let next = c.get(i + 1).copied();
        match ch {
            '\\' => {
                if next == Some('\n') {
                    i += 2;
                    continue;
                }
                if let Some(n) = next {
                    word.push(n);
                }
                in_word = true;
                i += 2;
                continue;
            }
            '\'' => {
                let mut j = i + 1;
                while j < c.len() && c[j] != '\'' {
                    word.push(c[j]);
                    j += 1;
                }
                if j >= c.len() {
                    return None;
                }
                in_word = true;
                i = j + 1;
                continue;
            }
            '"' => {
                let mut j = i + 1;
                loop {
                    if j >= c.len() {
                        return None;
                    }
                    match c[j] {
                        '"' => break,
                        '\\' => {
                            if let Some(&n) = c.get(j + 1) {
                                word.push(n);
                            }
                            j += 2;
                        }
                        '$' if c.get(j + 1) == Some(&'(') => {
                            let (text, end) = balanced(&c, j + 2)?;
                            word.push_str(&format!("$({text})"));
                            if is_arith(&text) {
                                inner.extend(arith_subs(&text));
                            } else {
                                inner.push(text);
                            }
                            j = end + 1;
                        }
                        '$' if c.get(j + 1) == Some(&'{') => {
                            let end = brace_end(&c, j + 1)?;
                            word.extend(&c[j..=end]);
                            j = end + 1;
                        }
                        '`' => {
                            let end = (j + 1..c.len()).find(|&k| c[k] == '`')?;
                            let text: String = c[j + 1..end].iter().collect();
                            word.push_str(&format!("$({text})"));
                            inner.push(text);
                            j = end + 1;
                        }
                        x => {
                            word.push(x);
                            j += 1;
                        }
                    }
                }
                in_word = true;
                i = j + 1;
                continue;
            }
            '$' if next == Some('(') => {
                let (text, end) = balanced(&c, i + 2)?;
                word.push_str(&format!("$({text})"));
                if is_arith(&text) {
                    inner.extend(arith_subs(&text));
                } else {
                    inner.push(text);
                }
                in_word = true;
                i = end + 1;
                continue;
            }
            '$' if next == Some('{') => {
                let end = brace_end(&c, i + 1)?;
                word.extend(&c[i..=end]);
                in_word = true;
                i = end + 1;
                continue;
            }
            '`' => {
                let end = (i + 1..c.len()).find(|&k| c[k] == '`')?;
                let text: String = c[i + 1..end].iter().collect();
                word.push_str(&format!("$({text})"));
                inner.push(text);
                in_word = true;
                i = end + 1;
                continue;
            }
            '#' if !in_word => {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            // an array assignment `a=(x y)`: the elements are the value
            '(' if in_word && word.ends_with('=') => {
                let (text, end) = balanced(&c, i + 1)?;
                word.push_str(&text);
                i = end + 1;
                continue;
            }
            // `(( ... ))` arithmetic: one opaque word
            '(' if !in_word && next == Some('(') => {
                let (text, end) = balanced(&c, i + 1)?;
                if is_arith(&text) {
                    inner.extend(arith_subs(&text));
                    fc!(false);
                    i = end + 1;
                    continue;
                }
                fc!(false);
            }
            ' ' | '\t' | '\r' => fw!(),
            '\n' => {
                fc!(false);
                // here-document bodies start on the next line
                if !heredocs.is_empty() {
                    let rest: String = c[i + 1..].iter().collect();
                    let mut consumed = 0;
                    let mut lines = rest.split_inclusive('\n');
                    let targets = std::mem::take(&mut hd_targets);
                    for (n, delim) in std::mem::take(&mut heredocs).into_iter().enumerate() {
                        let mut body = String::new();
                        let mut closed = false;
                        for line in lines.by_ref() {
                            consumed += line.chars().count();
                            if line.trim() == delim {
                                closed = true;
                                break;
                            }
                            body.push_str(line);
                        }
                        if !closed {
                            return None;
                        }
                        let t = targets
                            .get(n)
                            .copied()
                            .unwrap_or(cmds.len().saturating_sub(1));
                        if let Some(owner) = cmds.get_mut(t) {
                            owner.heredocs.push(body);
                        }
                    }
                    i += consumed;
                }
            }
            '&' if next == Some('>') => {
                fw!();
                next_kind = Next::Redirect;
                i += 2;
                if c.get(i) == Some(&'>') {
                    i += 1;
                }
                continue;
            }
            '|' => {
                // `||` is a list, `|` a pipe
                if next == Some('|') {
                    fc!(false);
                    i += 2;
                    continue;
                }
                fc!(true);
                if next == Some('&') {
                    i += 1;
                }
            }
            ';' | '&' | '(' | ')' => fc!(false),
            '>' => {
                // a digit just before (`2>`) is the fd, not a word
                if in_word && word.chars().all(|x| x.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                fw!();
                i += 1;
                if c.get(i) == Some(&'(') {
                    // output process substitution
                    let (text, end) = balanced(&c, i + 1)?;
                    inner.push(text);
                    i = end + 1;
                    continue;
                }
                if matches!(c.get(i), Some('>') | Some('|')) {
                    i += 1;
                }
                if c.get(i) == Some(&'&') {
                    // `>&2`, `2>&1`, `>&-` duplicate a descriptor; `>& file`
                    // writes the file
                    let mut j = i + 1;
                    while j < c.len() && (c[j].is_ascii_digit() || c[j] == '-') {
                        j += 1;
                    }
                    let dup = j > i + 1
                        && c.get(j)
                            .map_or(true, |x| x.is_whitespace() || ";&|)".contains(*x));
                    if dup {
                        i = j;
                        continue;
                    }
                    i += 1;
                }
                while c.get(i) == Some(&' ') {
                    i += 1;
                }
                if c.get(i) == Some(&'>') && c.get(i + 1) == Some(&'(') {
                    // `> >(tee x)`
                    let (text, end) = balanced(&c, i + 2)?;
                    inner.push(text);
                    i = end + 1;
                    continue;
                }
                next_kind = Next::Redirect;
                continue;
            }
            '<' => {
                fw!();
                if next == Some('<') && c.get(i + 2) == Some(&'<') {
                    next_kind = Next::HereString;
                    i += 3;
                    continue;
                }
                if next == Some('<') {
                    i += 2;
                    if c.get(i) == Some(&'-') {
                        i += 1;
                    }
                    while i < c.len() && (c[i] == ' ' || c[i] == '\t') {
                        i += 1;
                    }
                    let mut delim = String::new();
                    while i < c.len() && !c[i].is_whitespace() && !";&|<>()".contains(c[i]) {
                        if !"'\"\\".contains(c[i]) {
                            delim.push(c[i]);
                        }
                        i += 1;
                    }
                    if delim.is_empty() {
                        return None;
                    }
                    heredocs.push(delim);
                    cur.want_heredocs += 1;
                    continue;
                }
                if next == Some('(') {
                    // input process substitution
                    let (text, end) = balanced(&c, i + 2)?;
                    inner.push(text);
                    cur.procsub_in = true;
                    i = end + 1;
                    continue;
                }
                // an input file is read, not written
                next_kind = Next::Drop;
                i += 1;
                continue;
            }
            x => {
                word.push(x);
                in_word = true;
            }
        }
        i += 1;
    }
    fc!(false);
    if !heredocs.is_empty() {
        return None; // a here-document with no body line
    }
    for t in inner {
        cmds.extend(parse(&t, depth + 1)?);
    }
    Some(cmds)
}

/// The index of the `}` closing the `{` at `i` (`${...}`).
fn brace_end(c: &[char], i: usize) -> Option<usize> {
    let mut d = 0;
    let mut j = i;
    while j < c.len() {
        match c[j] {
            '\\' => j += 1,
            '{' => d += 1,
            '}' => {
                d -= 1;
                if d == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}

/// Variables: a path naming `p`, an unknown value, or a placed value.
#[derive(Clone, Debug)]
enum Var {
    Tainted,
    Unknown,
    Clean(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Cwd {
    /// workspace-relative, normalized; "" is the root
    Path(String),
    Outside,
    Unknown,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap())
}

/// Commands that write their arguments or working directory whatever their
/// options.
fn is_writer(name: &str) -> bool {
    matches!(
        name,
        "rm" | "rmdir"
            | "unlink"
            | "mv"
            | "cp"
            | "ln"
            | "install"
            | "rsync"
            | "scp"
            | "tar"
            | "bsdtar"
            | "cpio"
            | "pax"
            | "unzip"
            | "zip"
            | "7z"
            | "7za"
            | "ar"
            | "gzip"
            | "gunzip"
            | "bzip2"
            | "bunzip2"
            | "xz"
            | "unxz"
            | "zstd"
            | "unzstd"
            | "curl"
            | "wget"
            | "svn"
            | "hg"
            | "patch"
            | "touch"
            | "mkdir"
            | "mkfifo"
            | "tee"
            | "dd"
            | "truncate"
            | "chmod"
            | "chown"
            | "shred"
            | "split"
            | "csplit"
            | "rename"
            | "dos2unix"
            | "unix2dos"
            | "git-lfs"
            | "aws"
            | "gsutil"
            | "rclone"
            | "az"
            | "gcloud"
            | "oras"
    )
}

/// git subcommands that write nothing in the work tree.
fn git_reads(sub: &str) -> bool {
    matches!(
        sub,
        "rev-parse"
            | "log"
            | "show"
            | "status"
            | "diff"
            | "describe"
            | "rev-list"
            | "ls-files"
            | "ls-remote"
            | "ls-tree"
            | "cat-file"
            | "merge-base"
            | "blame"
            | "shortlog"
            | "grep"
            | "for-each-ref"
            | "name-rev"
            | "version"
            | "help"
            | "var"
            | "check-ignore"
            | "verify-commit"
            | "verify-tag"
    )
}

/// Commands whose output is content, not a path (a substitution of theirs is
/// an opaque value).
fn is_reader(cmd: &[String]) -> bool {
    let Some((name, first)) = command_name(cmd) else {
        return false;
    };
    let args = &cmd[first.min(cmd.len())..];
    match name.as_str() {
        "cat" | "head" | "tail" | "tr" | "cut" | "uniq" | "date" | "uname" | "nproc" | "whoami"
        | "hostname" | "id" | "jq" => true,
        // grep -l/-L/-r/-H/-c print file names
        "grep" | "egrep" | "fgrep" => !args.iter().any(|a| {
            (a.starts_with('-')
                && !a.starts_with("--")
                && a.contains(['l', 'L', 'r', 'R', 'H', 'c']))
                || a.starts_with("--files")
                || a.starts_with("--recursive")
                || a.starts_with("--with-filename")
                || a.starts_with("--count")
        }),
        "git" => {
            git_read_only(args)
                && git_sub(args).is_some_and(|s| {
                    matches!(
                        s.as_str(),
                        "rev-parse" | "describe" | "log" | "show" | "rev-list"
                    )
                })
        }
        "sed" => !args.iter().any(|a| sed_in_place(a)),
        _ => false,
    }
}

/// git's subcommand after its global options (`-C x`, `-c k=v`).
fn git_sub(args: &[String]) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if matches!(a, "-C" | "-c" | "--git-dir" | "--work-tree" | "--namespace") {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(a.to_ascii_lowercase());
    }
    None
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "busybox"];

/// Interpreters that run inline code (`python3.12`, `nodejs`, `bun`, ...).
fn is_interpreter(name: &str) -> bool {
    static R: OnceLock<Regex> = OnceLock::new();
    re(
        &R,
        r"^(python[0-9.]*|pypy[0-9.]*|perl[0-9.]*|ruby[0-9.]*|node|nodejs|deno|bun|php[0-9.]*|lua[0-9.]*|luajit|pwsh|powershell|tclsh|rscript|osascript|groovy|jshell)$",
    )
    .is_match(name)
}

/// Commands that run the rest of their words as another command.
fn is_wrapper(name: &str) -> bool {
    matches!(
        name,
        "sudo"
            | "doas"
            | "env"
            | "command"
            | "exec"
            | "nice"
            | "nohup"
            | "builtin"
            | "stdbuf"
            | "timeout"
            | "ionice"
            | "chronic"
            | "setsid"
            | "flock"
            | "unbuffer"
            | "xvfb-run"
            | "time"
            | "strace"
            | "ltrace"
            | "watch"
            | "retry"
            | "chroot"
            | "nsenter"
            | "unshare"
            | "runuser"
            | "su"
    )
}

fn basename(w: &str) -> String {
    let b = w.rsplit('/').next().unwrap_or(w).to_ascii_lowercase();
    b.strip_suffix(".exe").map(str::to_string).unwrap_or(b)
}

/// The command's name (basename, lowercased) and the index of its first
/// argument, after assignments, keywords and `function NAME`. A wrapper is
/// returned as the name: `cmd_writes` then scans every word after it.
fn command_name(words: &[String]) -> Option<(String, usize)> {
    let assign = re_assign();
    let mut skip_next = false;
    for (i, w) in words.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if assign.is_match(w) {
            continue;
        }
        let base = basename(w);
        if base == "function" {
            skip_next = true;
            continue;
        }
        if matches!(
            base.as_str(),
            "!" | "{"
                | "}"
                | "if"
                | "then"
                | "else"
                | "elif"
                | "fi"
                | "do"
                | "done"
                | "while"
                | "until"
        ) {
            continue;
        }
        return Some((base, i + 1));
    }
    None
}

fn re_assign() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^[A-Za-z_][A-Za-z0-9_]*(\[[^\]]*\])?\+?=")
}

struct Scan {
    p: String,
    p_parts: Vec<String>,
    /// `p` as a path component (see `aligned`)
    al: Regex,
    /// `p` as a word in inline code (quotes and brackets around it)
    code_al: Regex,
    vars: BTreeMap<String, Var>,
    cwds: Vec<Cwd>,
}

/// Fold `.` and `..`; `None` above the root.
fn fold(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

const GLOB: [char; 4] = ['*', '?', '[', '{'];

/// Shell glob match of one component, linear in time (no backtracking
/// blow-up); a `[...]` class or `{a,b}` brace matches any text
/// (conservative).
fn glob_comp(pat: &str, s: &str) -> bool {
    if pat.contains('{') {
        return true;
    }
    if pat.len() > 512 {
        return true;
    }
    // classes become `?`
    let mut p: Vec<char> = Vec::new();
    let mut it = pat.chars().peekable();
    while let Some(c) = it.next() {
        if c == '[' {
            for d in it.by_ref() {
                if d == ']' {
                    break;
                }
            }
            p.push('?');
        } else {
            p.push(c);
        }
    }
    let t: Vec<char> = s.chars().collect();
    // iterative wildcard match with one backtrack point
    let (mut i, mut j) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(s) = star {
            i = s + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

impl Scan {
    /// `full` (workspace-relative, folded) is at, under or above `p` (the
    /// root aside), or a glob in it covers `p`.
    fn at_or_under(&self, full: &str) -> bool {
        let parts: Vec<&str> = full.split('/').filter(|c| !c.is_empty()).collect();
        if parts.is_empty() {
            return false; // the root: a documented limit
        }
        let n = parts.len().min(self.p_parts.len());
        (0..n).all(|i| glob_comp(parts[i], &self.p_parts[i]))
    }

    /// The word with expressions and variables replaced by markers or values.
    fn normalize(&self, word: &str) -> String {
        static EXPR: OnceLock<Regex> = OnceLock::new();
        static VAR: OnceLock<Regex> = OnceLock::new();
        static SUB: OnceLock<Regex> = OnceLock::new();
        let expr = re(&EXPR, r"\$\{\{\s*([^}]*?)\s*\}\}");
        let var = re(
            &VAR,
            r"\$(?:\{([#!]?)([A-Za-z_][A-Za-z0-9_]*)([^}]*)\}|([A-Za-z_][A-Za-z0-9_]*)|([0-9@*])|\{[^}]*\})",
        );
        let sub = re(&SUB, r"\$\(([^()]*(?:\([^()]*\)[^()]*)*)\)");
        // command substitutions: a reader's output is opaque content
        let w = sub.replace_all(word, |c: &regex::Captures| {
            let text = &c[1];
            if is_arith(text) {
                return OPAQUE.to_string();
            }
            match parse(text, 1) {
                Some(cmds) if !cmds.is_empty() && cmds.iter().all(|cmd| is_reader(&cmd.words)) => {
                    OPAQUE.to_string()
                }
                _ => format!(" {text} "),
            }
        });
        let w = expr.replace_all(&w, |c: &regex::Captures| {
            let e: String = c[1].chars().filter(|c| !c.is_whitespace()).collect();
            let e = e.to_ascii_lowercase();
            match e.as_str() {
                "github.workspace" | "env.github_workspace" => ROOT.to_string(),
                "runner.temp" | "runner.tool_cache" | "env.runner_temp" => OUTSIDE.to_string(),
                "github.sha"
                | "github.run_id"
                | "github.run_number"
                | "github.run_attempt"
                | "github.event_name"
                | "github.ref"
                | "github.ref_name"
                | "github.head_ref"
                | "github.base_ref"
                | "github.repository"
                | "github.actor"
                | "runner.os"
                | "runner.arch"
                | "github.event.pull_request.number"
                | "github.event.pull_request.head.sha"
                | "github.event.pull_request.base.sha" => OPAQUE.to_string(),
                _ => UNKNOWN.to_string(),
            }
        });
        let hay = w.clone();
        let w = var.replace_all(&hay, |c: &regex::Captures| {
            if c.get(5).is_some() {
                return UNKNOWN.to_string(); // $1, $@, $*
            }
            let Some(name) = c.get(2).or(c.get(4)).map(|m| m.as_str()) else {
                return UNKNOWN.to_string(); // ${...} not a name
            };
            let prefix = c.get(1).map_or("", |m| m.as_str());
            if prefix == "!" {
                return UNKNOWN.to_string(); // indirect expansion
            }
            if prefix == "#" {
                return OPAQUE.to_string(); // a length
            }
            // an operator's word (`${X:-w}`, `${X/a/w}`) may be the result
            let op = c.get(3).map_or("", |m| m.as_str());
            let operand = op.trim_start_matches(|ch: char| ":-+=?#%/^,@".contains(ch));
            let operand = if op.starts_with('[') { "" } else { operand };
            let value = if let Some(v) = self.vars.get(name) {
                match v {
                    Var::Tainted => TAINT.to_string(),
                    Var::Unknown => UNKNOWN.to_string(),
                    Var::Clean(s) => s.clone(),
                }
            } else {
                match name {
                    "GITHUB_WORKSPACE" => ROOT.to_string(),
                    "RUNNER_TEMP"
                    | "RUNNER_TOOL_CACHE"
                    | "GITHUB_ENV"
                    | "GITHUB_OUTPUT"
                    | "GITHUB_PATH"
                    | "GITHUB_STEP_SUMMARY"
                    | "GITHUB_STATE"
                    | "GITHUB_EVENT_PATH" => OUTSIDE.to_string(),
                    "GITHUB_SHA" | "GITHUB_REF" | "GITHUB_REF_NAME" | "GITHUB_HEAD_REF"
                    | "GITHUB_BASE_REF" | "GITHUB_REPOSITORY" | "GITHUB_RUN_ID"
                    | "GITHUB_RUN_NUMBER" | "GITHUB_ACTOR" | "GITHUB_EVENT_NAME" | "RUNNER_OS"
                    | "RUNNER_ARCH" | "CI" => OPAQUE.to_string(),
                    // $HOME/.cache is outside; $HOME itself contains the workspace
                    "HOME" if op.is_empty() && hay[c.get(0).unwrap().end()..].starts_with("/.") => {
                        OUTSIDE.to_string()
                    }
                    _ => UNKNOWN.to_string(),
                }
            };
            if operand.is_empty() {
                value
            } else {
                format!("{value} {}", self.normalize(operand))
            }
        });
        let mut w = w.replace('\\', "/").to_lowercase();
        if w.starts_with('~') && !w.starts_with("~/.") {
            w = UNKNOWN.to_string();
        }
        w
    }

    /// `p` named as a path component anywhere in `w` (a normalized word).
    fn aligned(&self, w: &str) -> bool {
        self.al.is_match(w)
    }

    /// A written word (an argument or a redirection target) may be at, under
    /// or above `p`.
    fn word_hits(&self, word: &str) -> bool {
        let n = self.normalize(word);
        let mut segs = vec![n.clone()];
        if let Some((_, tail)) = n.split_once('=') {
            segs.push(tail.to_string());
        }
        segs.into_iter().any(|s| self.seg_hits(&s))
    }

    fn seg_hits(&self, s: &str) -> bool {
        static URL: OnceLock<Regex> = OnceLock::new();
        let url = re(&URL, r"^[a-z][a-z0-9+.-]*://|^[\w.-]+@[\w.-]+:");
        if s.contains(TAINT) || s.contains(UNKNOWN) {
            return true;
        }
        if url.is_match(s) || s == "/dev/null" {
            return false;
        }
        if self.aligned(s) {
            return true;
        }
        if let Some(rest) = s.strip_prefix(ROOT) {
            return fold(rest).is_some_and(|f| self.at_or_under(&f));
        }
        if s.starts_with('/') || s.starts_with(OUTSIDE) || s.starts_with(OPAQUE) {
            return false;
        }
        static DRIVE: OnceLock<Regex> = OnceLock::new();
        if re(&DRIVE, r"^[a-z]:").is_match(s) {
            return false; // absolute on Windows; `aligned` covered the name
        }
        self.cwds.iter().any(|c| match c {
            Cwd::Path(base) => fold(&format!("{base}/{s}")).is_some_and(|f| self.at_or_under(&f)),
            Cwd::Outside => false,
            Cwd::Unknown => true,
        })
    }

    /// Where a `cd` (or a `working-directory:`) to `word` may land.
    fn place(&self, word: &str) -> Vec<Cwd> {
        let s = self.normalize(word);
        if s.contains(TAINT) {
            return vec![Cwd::Path(self.p.clone())];
        }
        if s.contains(UNKNOWN)
            || s.contains(GLOB)
            || (s.split('/').any(|c| c == "..") && !s.starts_with(ROOT))
        {
            return vec![Cwd::Unknown];
        }
        if self.aligned(&s) {
            return vec![Cwd::Path(self.p.clone())];
        }
        if let Some(rest) = s.strip_prefix(ROOT) {
            return vec![fold(rest).map_or(Cwd::Outside, Cwd::Path)];
        }
        if s.starts_with('/') || s.starts_with(OUTSIDE) || s.starts_with(OPAQUE) || s.contains(':')
        {
            return vec![Cwd::Outside];
        }
        self.cwds
            .iter()
            .map(|c| match c {
                Cwd::Path(base) => fold(&format!("{base}/{s}")).map_or(Cwd::Outside, Cwd::Path),
                other => other.clone(),
            })
            .collect()
    }

    fn assign(&mut self, name: &str, value: &str) {
        let name = name.split('[').next().unwrap_or(name).trim_end_matches('+');
        let s = self.normalize(value);
        let v = if s.contains(TAINT) || (!s.contains(UNKNOWN) && self.seg_hits(&s)) {
            Var::Tainted
        } else if s.contains(UNKNOWN) {
            Var::Unknown
        } else {
            Var::Clean(s)
        };
        // once a path naming `p`, always (a later reassignment may not run)
        if !matches!(self.vars.get(name), Some(Var::Tainted)) {
            self.vars.insert(name.to_string(), v);
        }
    }

    fn unknown(&mut self, name: &str) {
        if !matches!(self.vars.get(name), Some(Var::Tainted)) {
            self.vars.insert(name.to_string(), Var::Unknown);
        }
    }

    /// Any cwd at, under or above `p` (the root aside), or unknown.
    fn in_p(&self) -> bool {
        self.cwds.iter().any(|c| match c {
            Cwd::Path(b) if !b.is_empty() => self.at_or_under(b),
            Cwd::Unknown => true,
            _ => false,
        })
    }

    fn code_hits(&self, code: &str) -> bool {
        let n = self.normalize(code);
        n.contains(TAINT) || n.contains(UNKNOWN) || self.aligned(&n) || self.code_al.is_match(&n)
    }

    /// Run `script` as shell code in this scan; an unparseable one hits.
    fn script(&mut self, script: &str, depth: usize) -> bool {
        match parse(script, depth + 1) {
            Some(inner) => self.cmds(&inner, depth + 1),
            None => true,
        }
    }

    /// The words a writer in this command may write, if it holds one.
    /// `heads` are the command-name positions: the name, or every word after
    /// a wrapper (whose own options cannot be told from the command).
    fn writer_targets(&self, w: &[String], nw: &[String], heads: &[usize]) -> Option<Vec<String>> {
        let start = w.iter().take_while(|x| re_assign().is_match(x)).count();
        let all = |k: usize| w[k..].to_vec();
        // values of `flags` (`-B dir`, `--prefix=dir`)
        let values = |k: usize, flags: &[&str]| -> Vec<String> {
            let mut out = Vec::new();
            for (j, a) in w[k + 1..].iter().enumerate() {
                for f in flags {
                    if a == f {
                        out.extend(w.get(k + 2 + j).cloned());
                    } else if let Some(v) = a.strip_prefix(&format!("{f}=")) {
                        out.push(v.to_string());
                    } else if f.len() == 2
                        && a.starts_with(f)
                        && a.len() > 2
                        && !f.starts_with("--")
                    {
                        out.push(a[2..].to_string());
                    }
                }
            }
            out
        };
        for &k in heads {
            let Some(word) = nw.get(k) else { continue };
            // a command name held in an unknown variable may be anything
            if k == start && word.contains(UNKNOWN) {
                return Some(all(k));
            }
            let name = basename(word);
            let rest = &nw[k + 1..];
            let raw = &w[k + 1..];
            let sub = rest
                .iter()
                .find(|a| !a.starts_with('-'))
                .map(String::as_str);
            let t = match name.as_str() {
                n if is_writer(n) => Some(all(k)),
                // git options are case-sensitive (`-C` vs `-c`): read the raw
                // words; an environment assignment (GIT_EXTERNAL_DIFF=...)
                // can make any subcommand run code
                "git" if start > 0 || !git_read_only(raw) => {
                    let mut t = match git_sub(raw).as_deref() {
                        // refs and remotes, not work-tree paths
                        Some(
                            "fetch" | "push" | "remote" | "tag" | "notes" | "config" | "branch",
                        ) => values(k, &["-C", "--git-dir", "--work-tree"]),
                        _ => all(k),
                    };
                    for v in [
                        "GIT_DIR",
                        "GIT_WORK_TREE",
                        "GIT_COMMON_DIR",
                        "GIT_INDEX_FILE",
                    ] {
                        if self.vars.contains_key(v) {
                            t.push(format!("${v}"));
                        }
                    }
                    Some(t)
                }
                "git" => None,
                "sed" | "gsed" if rest.iter().any(|a| sed_in_place(a)) => Some(all(k)),
                "perl" | "ruby" if rest.iter().any(|a| short_flag(a, 'i')) => Some(all(k)),
                "yq" if rest
                    .iter()
                    .any(|a| short_flag(a, 'i') || a.starts_with("--inplace")) =>
                {
                    Some(all(k))
                }
                "awk" | "gawk" | "mawk" | "nawk"
                    if rest.iter().any(|a| {
                        a == "-i"
                            || a.contains('>')
                            || a.contains("system")
                            || a.contains("getline")
                    }) =>
                {
                    Some(all(k))
                }
                "sort"
                    if rest
                        .iter()
                        .any(|a| short_flag(a, 'o') || a.starts_with("--output")) =>
                {
                    Some(all(k))
                }
                "find"
                    if rest.iter().any(|a| {
                        matches!(
                            a.as_str(),
                            "-delete"
                                | "-exec"
                                | "-execdir"
                                | "-ok"
                                | "-okdir"
                                | "-fprint"
                                | "-fprint0"
                                | "-fprintf"
                                | "-fls"
                        )
                    }) =>
                {
                    Some(all(k))
                }
                // gh writes its output flags, a clone, or the cwd
                // (`pr checkout`, `release download` without -D)
                "gh" if rest.iter().any(|a| {
                    matches!(
                        a.as_str(),
                        "checkout"
                            | "sync"
                            | "clone"
                            | "download"
                            | "-d"
                            | "--dir"
                            | "-o"
                            | "--output"
                            | "--clobber"
                    ) || a.starts_with("--dir=")
                        || a.starts_with("--output=")
                }) =>
                {
                    Some(gh_targets(&w[k..]))
                }
                // cmake writes its build and install trees (the cwd without
                // -B), or anything under -E
                "cmake" => Some(if rest.iter().any(|a| a == "-e") {
                    all(k)
                } else {
                    let mut t = values(k, &["-B", "--build", "--prefix", "--install-prefix"]);
                    t.extend(raw.iter().filter_map(|a| {
                        a.strip_prefix("-D")
                            .filter(|d| {
                                d.starts_with("CMAKE_")
                                    && (d.contains("_DIR") || d.contains("PREFIX"))
                            })
                            .and_then(|d| d.split_once('=').map(|(_, v)| v.to_string()))
                    }));
                    t
                }),
                // docker and podman write through cp, mounts and exports;
                // build only reads its context
                "docker" | "podman" | "nerdctl"
                    if matches!(
                        sub,
                        Some(
                            "cp" | "run"
                                | "create"
                                | "compose"
                                | "container"
                                | "export"
                                | "save"
                                | "volume"
                        )
                    ) || (sub == Some("buildx")
                        && rest.iter().any(|a| a.starts_with("--output") || a == "-o")) =>
                {
                    Some(all(k))
                }
                _ => None,
            };
            if t.is_some() {
                return t;
            }
        }
        None
    }

    /// Whether these commands may write at, under or above `p`.
    fn cmds(&mut self, cmds: &[Cmd], depth: usize) -> bool {
        let mut hit = false;
        for cmd in cmds {
            for r in &cmd.redirects {
                if self.word_hits(r) {
                    hit = true;
                }
            }
            let w = &cmd.words;
            // assignments before the command, or the whole command
            for a in w.iter().take_while(|x| re_assign().is_match(x)) {
                let (k, v) = a.split_once('=').unwrap();
                self.assign(k, v);
            }
            // `${x:=v}` and `${x=v}` assign
            static DEFAULT: OnceLock<Regex> = OnceLock::new();
            let dflt = re(&DEFAULT, r"\$\{([A-Za-z_][A-Za-z0-9_]*):?=([^}]*)\}");
            for a in w {
                for c in dflt.captures_iter(a) {
                    let (k, v) = (c[1].to_string(), c[2].to_string());
                    self.assign(&k, &v);
                }
            }
            // `for x in ...`, also after `then`, `do`, `else`, `{`
            let fpos = w
                .iter()
                .take_while(|x| {
                    matches!(
                        x.as_str(),
                        "then" | "do" | "else" | "{" | "!" | "if" | "while" | "until" | "elif"
                    )
                })
                .count();
            if w.get(fpos).map(String::as_str) == Some("for") && w.len() > fpos + 1 {
                let rest = w
                    .iter()
                    .skip(fpos + 3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" ");
                let name = w[fpos + 1].clone();
                if rest.is_empty() {
                    self.unknown(&name);
                } else {
                    self.assign(&name, &rest);
                }
                continue;
            }
            let Some((name, first)) = command_name(w) else {
                continue;
            };
            // shell builtins may sit behind `builtin`, `command` and the like
            let (eff, eff_first) = builtin_name(w).unwrap_or((name.clone(), first));
            let args = &w[eff_first.min(w.len())..];
            match eff.as_str() {
                "export" | "declare" | "local" | "readonly" | "typeset" => {
                    let nameref = args.iter().any(|a| short_flag(a, 'n'));
                    for a in args.iter().filter(|a| !a.starts_with('-')) {
                        match a.split_once('=') {
                            Some((k, v)) if !nameref => self.assign(k, v),
                            Some((k, _)) => self.unknown(k),
                            None if nameref => self.unknown(a),
                            None => {}
                        }
                    }
                    continue;
                }
                "read" | "mapfile" | "readarray" | "getopts" => {
                    for a in args.iter().filter(|a| !a.starts_with('-')) {
                        self.unknown(a);
                    }
                    continue;
                }
                "printf" => {
                    if let Some(k) = args.iter().position(|a| a == "-v") {
                        if let Some(n) = args.get(k + 1) {
                            self.unknown(n);
                        }
                    }
                }
                "cd" | "pushd" => {
                    let target = args.iter().find(|a| !a.starts_with('-'));
                    let new = match target {
                        Some(t) => self.place(t),
                        None => vec![Cwd::Outside], // $HOME
                    };
                    for c in new {
                        if !self.cwds.contains(&c) {
                            self.cwds.push(c);
                        }
                    }
                    continue;
                }
                _ => {}
            }
            let nw: Vec<String> = w.iter().map(|x| self.normalize(x)).collect();
            let h = first.saturating_sub(1);
            let heads: Vec<usize> = if is_wrapper(&name) {
                (h..w.len()).collect()
            } else {
                vec![h]
            };
            // a shell, eval or interpreter at a command position: its code runs
            // here (the first one only; nested ones are in its code)
            for &k in &heads {
                let b = basename(&nw[k]);
                let rest = &w[k + 1..];
                if SHELLS.contains(&b.as_str()) || b == "source" || b == "." {
                    if let Some(c) = rest.iter().position(|a| {
                        a.starts_with('-')
                            && !a.starts_with("--")
                            && a[1..].chars().all(|x| x.is_ascii_alphabetic())
                            && a.contains('c')
                    }) {
                        for code in rest[c + 1..].iter().filter(|a| !a.starts_with('-')) {
                            hit |= self.script(code, depth);
                        }
                    }
                    for body in cmd.heredocs.iter().chain(cmd.herestrings.iter()) {
                        hit |= self.script(body, depth);
                    }
                    // code from a process substitution (`bash <(curl ...)`)
                    if cmd.procsub_in {
                        hit = true;
                    }
                    if cmd.piped_in && stdin_is_code(rest) {
                        hit = true;
                    }
                    break;
                }
                if b == "eval" {
                    hit |= self.script(&rest.join(" "), depth);
                    break;
                }
                if is_interpreter(&b) {
                    // inline code, or code on stdin, here-documents and
                    // process substitutions: anything but a script file
                    if !script_file(rest) || has_inline(rest) {
                        for a in rest {
                            hit |= self.code_hits(a);
                        }
                    }
                    for body in cmd.heredocs.iter().chain(cmd.herestrings.iter()) {
                        hit |= self.code_hits(body);
                    }
                    if cmd.procsub_in || (cmd.piped_in && stdin_is_code(rest)) {
                        hit = true;
                    }
                    break;
                }
                // xargs and parallel take a writer's arguments from stdin
                if matches!(b.as_str(), "xargs" | "parallel")
                    && nw[k + 1..].iter().any(|x| {
                        let b = basename(x);
                        is_writer(&b)
                            || SHELLS.contains(&b.as_str())
                            || is_interpreter(&b)
                            || matches!(b.as_str(), "git" | "sed" | "gsed" | "find" | "eval")
                    })
                {
                    hit = true;
                }
            }
            if let Some(targets) = self.writer_targets(w, &nw, &heads) {
                if self.in_p() || targets.iter().any(|a| self.word_hits(a)) {
                    hit = true;
                }
            }
        }
        hit
    }
}

/// The first operand is a script file (a path with `/` or a script
/// extension), whose code is not in the command.
fn script_file(args: &[String]) -> bool {
    static EXT: OnceLock<Regex> = OnceLock::new();
    let ext = re(
        &EXT,
        r"(?i)\.(sh|bash|zsh|py|pl|rb|js|mjs|cjs|ts|ps1|php|lua|r|tcl)$",
    );
    args.iter()
        .find(|a| !a.starts_with('-') && !a.starts_with('+'))
        .is_some_and(|a| a != "-" && a != "/dev/stdin" && (a.contains('/') || ext.is_match(a)))
}

/// An inline-code flag (`-c`, `-e`, `-p`, `--eval=...`, `eval`).
fn has_inline(args: &[String]) -> bool {
    args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-c" | "-e"
                | "-E"
                | "-p"
                | "-r"
                | "--eval"
                | "--print"
                | "-Command"
                | "-command"
                | "eval"
        ) || a.starts_with("--eval=")
            || a.starts_with("--print=")
            || (a.starts_with('-')
                && !a.starts_with("--")
                && a[1..].chars().all(|x| x.is_ascii_alphabetic())
                && a.contains(['c', 'e', 'p']))
    })
}

fn short_flag(a: &str, f: char) -> bool {
    a.starts_with('-') && !a.starts_with("--") && a[1..].contains(f)
}

/// A shell or interpreter with these arguments reads its code from stdin:
/// no `-c`/`-e` code argument and no script-file operand (a path with `/` or
/// a script extension), or an explicit `-s`, `-` or `/dev/stdin`. Unsure
/// counts as code.
fn stdin_is_code(args: &[String]) -> bool {
    let inline = args.iter().any(|a| {
        matches!(
            a.as_str(),
            "-c" | "-e" | "-E" | "--eval" | "-Command" | "-command"
        ) || (a.starts_with('-')
            && !a.starts_with("--")
            && a[1..].chars().all(|x| x.is_ascii_alphabetic())
            && a.contains(['c', 'e']))
    });
    if inline {
        return false;
    }
    static EXT: OnceLock<Regex> = OnceLock::new();
    let ext = re(
        &EXT,
        r"(?i)\.(sh|bash|zsh|py|pl|rb|js|mjs|cjs|ts|ps1|php|lua)$",
    );
    let explicit = args.iter().any(|a| {
        a == "-"
            || a == "/dev/stdin"
            || (short_flag(a, 's') && a[1..].chars().all(|x| x.is_ascii_alphabetic()))
    });
    let file = args.iter().any(|a| {
        !a.starts_with('-') && !a.starts_with('+') && (a.contains('/') || ext.is_match(a))
    });
    explicit || !file
}

/// git with a subcommand that writes nothing, and no option that makes it
/// write a file or run a program (`-c` config, `--output`, `--ext-diff`,
/// `--upload-pack`, `-O` pager, ...).
fn git_read_only(args: &[String]) -> bool {
    let Some(sub) = git_sub(args) else {
        return false;
    };
    git_reads(&sub)
        && !args.iter().any(|a| {
            let a = a.as_str();
            matches!(a, "-c" | "-u" | "-p" | "--paginate")
                || a.starts_with("--config-env")
                || a.starts_with("--exec")
                || a.starts_with("--output")
                || a.starts_with("--upload-pack")
                || a.starts_with("-O")
                || a.starts_with("--open-files-in-pager")
                || a.starts_with("--ext-diff")
                || a.starts_with("--textconv")
                || a.starts_with("--receive-pack")
        })
}

/// The builtin a command runs behind `builtin`, `command`, `exec` and other
/// wrappers (and their options), for `cd`, `export`, `read` and `printf -v`.
fn builtin_name(words: &[String]) -> Option<(String, usize)> {
    let assign = re_assign();
    let mut wrapped = false;
    for (i, w) in words.iter().enumerate() {
        let b = basename(w);
        if assign.is_match(w) || matches!(b.as_str(), "!" | "{" | "then" | "else" | "do" | "time") {
            continue;
        }
        if matches!(
            b.as_str(),
            "builtin"
                | "command"
                | "exec"
                | "env"
                | "nice"
                | "nohup"
                | "sudo"
                | "doas"
                | "timeout"
                | "stdbuf"
                | "setsid"
        ) {
            wrapped = true;
            continue;
        }
        if wrapped && (w.starts_with('-') || w.chars().all(|c| c.is_ascii_digit() || c == '.')) {
            continue;
        }
        return Some((b, i + 1));
    }
    None
}

fn sed_in_place(a: &str) -> bool {
    a.starts_with("--in-place") || short_flag(a, 'i')
}

/// The words `gh` may write: the values of its output and directory flags,
/// and a `repo clone` destination (or the repository, whose name becomes the
/// directory). `-R owner/repo` and `repos/...` API paths are not paths.
fn gh_targets(words: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, a) in words.iter().enumerate() {
        if matches!(a.as_str(), "-D" | "--dir" | "-O" | "--output" | "-o") {
            out.extend(words.get(i + 1).cloned());
        }
        for f in ["--dir=", "--output="] {
            if let Some(v) = a.strip_prefix(f) {
                out.push(v.to_string());
            }
        }
        if a == "clone" {
            let rest: Vec<&String> = words[i + 1..]
                .iter()
                .filter(|x| !x.starts_with('-'))
                .collect();
            out.extend(rest.get(1).or(rest.first()).map(|s| s.to_string()));
        }
    }
    out
}

/// sieve#41: may `script`, run in `wd` with `env` (workflow, job, then step
/// variables, in order), write at, under or above the checkout path `p`?
pub fn may_write(script: &str, env: &[(String, String)], wd: &str, p: &str) -> bool {
    let p = p.to_lowercase();
    // a path component: separators, `=`/`:`/`,` (options, lists), braces
    let al = Regex::new(&format!(
        r#"(^|[/=:,{{\s"'{ROOT}{OUTSIDE}]){}(/|$|[\s,;:{{}}"'])"#,
        regex::escape(&p)
    ))
    .unwrap();
    let code_al = Regex::new(&format!(
        r"(^|[^a-z0-9_.-]){}([^a-z0-9_-]|$)",
        regex::escape(&p)
    ))
    .unwrap();
    let mut s = Scan {
        code_al,
        p_parts: p.split('/').map(str::to_string).collect(),
        p,
        al,
        vars: BTreeMap::new(),
        cwds: vec![Cwd::Path(String::new())],
    };
    for (k, v) in env {
        s.assign(k, v);
    }
    if !wd.trim().is_empty() {
        s.cwds = s.place(wd.trim());
    }
    let Some(cmds) = parse(script, 0) else {
        return true;
    };
    s.cmds(&cmds, 0)
}

#[cfg(test)]
mod tests {
    use super::may_write;

    fn w(script: &str) -> bool {
        may_write(script, &[], "", "gale")
    }

    #[test]
    fn reads_comments_and_branch_names_are_not_writes() {
        for s in [
            "# RETRY, POSIX-sh SAFE (gale#289).\ncurl -sSf https://sh.rustup.rs -o /tmp/r.sh",
            "REV=\"$(grep -v '^#' \"${GITHUB_WORKSPACE}/gale/.github/pin\" | tr -d '[:space:]')\"\ngit -C ws/zephyr fetch --depth 1 origin \"$REV\"",
            "west init -m https://github.com/x/zephyr.git --mr gale/sem-replacement ws",
            "git clone https://github.com/pulseengine/gale.git /tmp/g",
            "rm -rf /opt/toolchains/zephyr-sdk-* 2>/dev/null || true",
            "cd /mnt/ws && rm -rf build",
            "echo x >> \"$GITHUB_PATH\"",
            "git -C gale rev-parse HEAD",
            "cd gale && cargo test && git status",
            "gh api repos/pulseengine/gale/releases/latest",
            "gh release download -R pulseengine/gale -D /tmp/d",
            "n=$(( 1<<3 ))\nrm -rf /tmp/x",
            "(( n = 1<<3 ))\necho $n > /tmp/n",
            "echo hi >&2",
            "sort -u gale/list",
            "cat data.json | python3 tools/check.py",
            "git -C gale log --oneline -1",
            "git fetch origin gale/feature",
            "west sdk install -t arm-zephyr-eabi --personal-access-token \"${GITHUB_TOKEN}\"",
            "grep -q zip gale/Cargo.toml",
            "rg install gale/src",
            "cargo build --manifest-path gale/Cargo.toml --features tar",
            "cargo install --path gale",
            "echo cp gale",
            "cmake -S gale -B build && cmake --build build",
            "docker build -t img gale",
            "docker build -f gale/Dockerfile .",
            "python3 \"$GALE_ROOT/benches/analyze.py\" --gale /tmp/e.csv",
        ] {
            assert!(!w(s), "refused: {s:?}");
        }
        assert!(!may_write(
            "west build -s \"$GALE_ROOT/benches\" -d /tmp/b",
            &[("GALE_ROOT".into(), "${{ github.workspace }}/gale".into())],
            "",
            "gale"
        ));
    }

    #[test]
    fn writes_naming_the_path_count() {
        for s in [
            "rm -rf gale",
            "rm -rf GALE",
            "rm -rf ./gale/",
            "git -C gale checkout other",
            "cd gale && git checkout other",
            "cp -r /tmp/x gale/.github/actions/",
            "echo x > gale/.github/actions/a/action.yml",
            "cat <<EOF > gale/a\nx\nEOF",
            "D=gale; rm -rf \"$D\"",
            "rm -rf \"$UNSET\"/x",
            "bash -c 'rm -rf gale'",
            "echo \"$(rm -rf gale)\"",
            "bash <<'EOF'\nrm -rf gale\nEOF",
            "python3 -c \"import shutil; shutil.rmtree('gale')\"",
            "rm -rf g*",
            "(cd /tmp); rm -rf gale",
            "for d in gale/x; do rm -rf $d; done",
            "sed -i s/a/b/ gale/x",
            "rm -rf /home/runner/work/r/gale",
            "rsync -a /tmp/x/ --target=gale",
            "echo a#b; rm -rf gale",
            "echo 'unclosed",
            "ls gale | xargs rm -rf",
            "rm -rf ${{ inputs.dir }}",
            "rm -rf ~/work",
            "cd \"$X\" && git checkout y",
            "eval \"rm -rf gale\"",
            "rm -rf \"${GITHUB_WORKSPACE}/gale\"",
            "rm -rf ${{ github.workspace }}/g*",
            "find gale -name x -delete",
            "D=$(realpath gale); rm -rf \"$D\"",
            "rm -rf \\\n  gale",
            // review of 7206093
            "(( n = 1<<3 ))\nrm -rf gale",
            "bash --norc -c 'rm -rf gale'",
            "bash --noprofile --norc -eo pipefail -c 'rm -rf gale'",
            "sudo -u runner rm -rf gale",
            "timeout 5m rm -rf gale",
            "env -u FOO rm -rf gale",
            "flock /tmp/l rm -rf gale",
            "setsid rm -rf gale",
            "X=1; rm -rf \"${X:+gale}\"",
            "X=; rm -rf \"${X:-gale}\"",
            "X=x; rm -rf \"${X/x/gale}\"",
            "a=(gale); rm -rf \"${a[0]}\"",
            "a=(gale other); rm -rf \"${a[@]}\"",
            "D=x; printf -v D gale; rm -rf \"$D\"",
            "ref=GALE_DIR; GALE_DIR=gale; rm -rf \"${!ref}\"",
            "rm -rf {gale,other}/action.yml",
            "cp /tmp/a gale{,}/action.yml",
            "function clean { rm -rf gale; }; clean",
            "echo 'rm -rf gale' | bash",
            "bash <<< 'rm -rf gale'",
            "RM=rm; $RM -rf gale",
            "ls | xargs -I {} rm -rf {}",
            "ls | xargs -n1 rm -rf",
            "sed -i 's/a/b/' $(grep -rl foo gale)",
            "echo pwned >& gale/action.yml",
            "echo pwned >&gale/action.yml",
            "exec > >(tee gale/action.yml)",
            "yq -i '.a = 1' gale/action.yml",
            "gawk -i inplace '{print}' gale/action.yml",
            "awk '{print > \"gale/action.yml\"}' x",
            "ruby -pi -e 'x' gale/action.yml",
            "sort -o gale/action.yml x",
            "unlink gale/action.yml",
            "gsed -i s/a/b/ gale/action.yml",
            "dos2unix gale/action.yml",
            "cmake -E rm -rf gale",
            "docker cp c:/x gale",
            "aws s3 sync s3://b/x gale",
            "$UNSET -rf gale",
            "cat <<EOF\nno end",
            // commit security review of 99672bd
            "echo $(( $(rm -rf gale) + 1 ))",
            "(( $(rm -rf gale) ))",
            "((cd /tmp); rm -rf gale)",
            "X=$( (cd /tmp; rm -rf gale) )",
            "git -c core.pager=x log gale",
            "git log --output=gale/action.yml",
            "git diff --ext-diff gale",
            "GIT_EXTERNAL_DIFF=x git diff gale",
            "builtin cd gale; git checkout other",
            "command cd gale && git checkout other",
            "builtin export D=gale; rm -rf \"$D\"",
            "curl -sSf https://x.invalid/i.sh | bash -s -- --yes",
            "curl -sSf https://x.invalid/i.sh | bash -o pipefail",
            "cat code | python3 -",
            // second review of 99672bd
            "bash <<'EOF' && echo ok\nrm -rf gale\nEOF",
            "bash <<'EOF' | tee /tmp/log\nrm -rf gale\nEOF",
            "python3 - <<'EOF' | tee /tmp/log\nimport shutil; shutil.rmtree('gale')\nEOF",
            "x=$( (rm -rf gale) )",
            "echo \"$( (rm -rf gale) )\"",
            "((cd /tmp) ; rm -rf gale)",
            "GIT_WORK_TREE=gale git checkout -f other",
            "export GIT_WORK_TREE=gale\ngit checkout -f other",
            "export GIT_DIR=gale/.git; git reset --hard HEAD~1",
            "cd gale && gh pr checkout 123",
            "D=gale; X=; rm -rf \"${X:-$D}\"",
            "D=gale; X=1; rm -rf \"${X:+$D}\"",
            "python3.12 -c \"import shutil; shutil.rmtree('gale')\"",
            "node -p \"require('fs').rmSync('gale',{recursive:true})\"",
            "node --eval=\"require('fs').rmSync('gale')\"",
            "deno eval \"Deno.removeSync('gale',{recursive:true})\"",
            "bun -e \"require('fs').rmSync('gale')\"",
            "bash < <(echo 'rm -rf gale')",
            "source <(echo 'rm -rf gale')",
            "d=build; if true; then for d in gale; do rm -rf \"$d\"; done; fi",
            "d=x; : \"${d:=gale}\"; rm -rf \"$d\"",
            "git log -1 --output=gale/action.yml",
            "git diff --output=gale/action.yml HEAD~1",
            "docker run --rm -v \"$PWD/gale:/src\" img make",
            "cmake -E rm -rf gale",
            "cmake -S . -B gale/build",
        ] {
            assert!(w(s), "missed: {s:?}");
        }
        // a parent of a nested checkout path
        assert!(may_write("rm -rf tools", &[], "", "tools/gale"));
        assert!(may_write(
            "mv tools /tmp/t && mv /tmp/evil tools",
            &[],
            "",
            "tools/gale"
        ));
        // a working directory in, or above, the path
        assert!(may_write("git checkout other", &[], "gale", "gale"));
        assert!(may_write("gh pr checkout 123", &[], "gale", "gale"));
        assert!(may_write(
            "git checkout -f other",
            &[("GIT_WORK_TREE".into(), "gale".into())],
            "",
            "gale"
        ));
        assert!(may_write("git checkout other", &[], "a", "a/gale"));
        assert!(may_write(
            "git checkout other",
            &[],
            "${{ matrix.d }}",
            "gale"
        ));
        assert!(may_write(
            "rm -rf \"$R\"",
            &[("R".into(), "${{ github.workspace }}/gale".into())],
            "",
            "gale"
        ));
    }

    #[test]
    fn hostile_globs_finish() {
        let long = format!("rm -rf {}x", "*".repeat(200_000));
        let t = std::time::Instant::now();
        assert!(may_write(&long, &[], "", "a-much-longer-checkout-dir"));
        assert!(!may_write(
            "rm -rf **************x",
            &[],
            "",
            "a-much-longer-checkout-dir"
        ));
        assert!(
            t.elapsed().as_secs() < 5,
            "glob matching took {:?}",
            t.elapsed()
        );
    }
}
