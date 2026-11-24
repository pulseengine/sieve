//! sieve#41: whether a `run:` script may write at, under or above a checked-out
//! path `p` (sieve#36). Before, any file or git command anywhere in the script
//! plus the bare word `p` anywhere (a comment, an issue reference, a read, a
//! branch name) counted, so every job that builds from its own checkout
//! refused. Now the script is split into simple commands, comments are
//! dropped, and a rewrite needs one command that writes (a file or git
//! command, an in-place edit, a redirection) and names `p` as a path:
//! literally, through a variable assigned a path naming it, or by running in
//! it. What cannot be placed counts as naming every path (fail closed): an
//! unassigned variable or an expression in a written path, a `cd` to one, an
//! unparseable script (an unclosed quote or substitution), and `xargs` feeding
//! a writer.
//!
//! Not modelled (limits, as before): a program or script that writes into `p`
//! on its own (`make`, `west`, `./setup.sh`), a write at the workspace root
//! without naming `p` (`git clean -fdx`, `rm -rf .`), and a variable set
//! through `$GITHUB_ENV` in an earlier step (it is unassigned here, so it
//! counts when written to).

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

/// Split a script into simple commands. `None` when a quote, a substitution
/// or a parameter expansion is not closed.
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
    let mut redirect_next = false;
    let mut drop_next = false;
    let mut heredocs: Vec<String> = Vec::new();
    let mut i = 0;
    let finish_word = |cur: &mut Cmd,
                       word: &mut String,
                       in_word: &mut bool,
                       redirect_next: &mut bool,
                       drop_next: &mut bool| {
        if *in_word {
            if *redirect_next {
                cur.redirects.push(std::mem::take(word));
            } else if *drop_next {
                word.clear();
            } else {
                cur.words.push(std::mem::take(word));
            }
            *redirect_next = false;
            *drop_next = false;
            *in_word = false;
        }
    };
    macro_rules! fw {
        () => {
            finish_word(
                &mut cur,
                &mut word,
                &mut in_word,
                &mut redirect_next,
                &mut drop_next,
            )
        };
    }
    macro_rules! fc {
        () => {{
            fw!();
            if !cur.words.is_empty() || !cur.redirects.is_empty() {
                cmds.push(std::mem::take(&mut cur));
            }
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
                            inner.push(text);
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
                inner.push(text);
                in_word = true;
                i = end + 1;
                continue;
            }
            '$' if next == Some('{') => {
                let mut d = 0;
                let mut j = i + 1;
                loop {
                    if j >= c.len() {
                        return None;
                    }
                    match c[j] {
                        '{' => d += 1,
                        '}' => {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                word.extend(&c[i..=j]);
                in_word = true;
                i = j + 1;
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
            ' ' | '\t' | '\r' => fw!(),
            '\n' => {
                fc!();
                // here-document bodies start on the next line
                if !heredocs.is_empty() {
                    let rest: String = c[i + 1..].iter().collect();
                    let mut consumed = 0;
                    let mut lines = rest.split_inclusive('\n');
                    for delim in std::mem::take(&mut heredocs) {
                        let mut body = String::new();
                        for line in lines.by_ref() {
                            consumed += line.chars().count();
                            if line.trim() == delim {
                                break;
                            }
                            body.push_str(line);
                        }
                        if let Some(last) = cmds.last_mut() {
                            last.heredocs.push(body);
                        }
                    }
                    i += consumed;
                }
            }
            '&' if next == Some('>') => {
                fw!();
                redirect_next = true;
                i += 2;
                if c.get(i) == Some(&'>') {
                    i += 1;
                }
                continue;
            }
            ';' | '&' | '|' | '(' | ')' => fc!(),
            '>' => {
                // a digit just before (`2>`) is the fd, not a word
                if in_word && word.chars().all(|x| x.is_ascii_digit()) {
                    word.clear();
                    in_word = false;
                }
                fw!();
                i += 1;
                if matches!(c.get(i), Some('>') | Some('|')) {
                    i += 1;
                }
                if c.get(i) == Some(&'&') {
                    // fd duplication (`>&2`, `2>&1`)
                    i += 1;
                    while i < c.len() && (c[i].is_ascii_digit() || c[i] == '-') {
                        i += 1;
                    }
                } else {
                    redirect_next = true;
                }
                continue;
            }
            '<' => {
                fw!();
                if next == Some('<') && c.get(i + 2) == Some(&'<') {
                    // here-string: the next word is data
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
                    heredocs.push(delim);
                    continue;
                }
                if next == Some('(') {
                    // process substitution
                    let (text, end) = balanced(&c, i + 2)?;
                    inner.push(text);
                    i = end + 1;
                    continue;
                }
                // an input file is read, not written
                drop_next = true;
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
    fc!();
    for t in inner {
        cmds.extend(parse(&t, depth + 1)?);
    }
    Some(cmds)
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

/// Commands whose arguments or working directory may be written.
fn is_writer(name: &str) -> bool {
    matches!(
        name,
        "git"
            | "rm"
            | "rmdir"
            | "mv"
            | "cp"
            | "ln"
            | "install"
            | "rsync"
            | "tar"
            | "unzip"
            | "7z"
            | "gzip"
            | "gunzip"
            | "bzip2"
            | "bunzip2"
            | "xz"
            | "unxz"
            | "zstd"
            | "curl"
            | "wget"
            | "gh"
            | "svn"
            | "hg"
            | "patch"
            | "touch"
            | "mkdir"
            | "tee"
            | "dd"
            | "truncate"
            | "chmod"
            | "chown"
            | "shred"
            | "git-lfs"
    )
}

/// Commands whose output is content, not a path (a substitution of theirs is
/// an opaque value).
fn is_reader(name: &str) -> bool {
    matches!(
        name,
        "grep"
            | "egrep"
            | "fgrep"
            | "rg"
            | "cat"
            | "head"
            | "tail"
            | "tr"
            | "wc"
            | "cut"
            | "sort"
            | "uniq"
            | "awk"
            | "jq"
            | "yq"
            | "sha256sum"
            | "sha1sum"
            | "md5sum"
            | "date"
            | "uname"
            | "nproc"
            | "whoami"
            | "hostname"
            | "id"
    )
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh"];
const INTERPRETERS: &[&str] = &[
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "pwsh",
    "powershell",
    "deno",
];

/// The command's name (basename, lowercased) and the index of its first
/// argument, after assignments, keywords and wrappers.
fn command_name(words: &[String]) -> Option<(String, usize)> {
    let assign = re_assign();
    let mut wrapper = false;
    for (i, w) in words.iter().enumerate() {
        if assign.is_match(w) {
            continue;
        }
        let base = w.rsplit('/').next().unwrap_or(w).to_ascii_lowercase();
        let base = base.strip_suffix(".exe").unwrap_or(&base).to_string();
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
                | "time"
        ) {
            continue;
        }
        if matches!(
            base.as_str(),
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
        ) {
            wrapper = true;
            continue;
        }
        if wrapper && (w.starts_with('-') || w.chars().all(|c| c.is_ascii_digit() || c == '.')) {
            continue;
        }
        return Some((base, i + 1));
    }
    None
}

fn re_assign() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"^[A-Za-z_][A-Za-z0-9_]*\+?=")
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

fn glob_comp(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|k| go(&p[1..], &t[k..])),
            Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some('[') => {
                // a class: any one character (conservative)
                let close = p.iter().position(|&c| c == ']').unwrap_or(0);
                !t.is_empty() && go(&p[close + 1..], &t[1..])
            }
            Some(&c) => t.first() == Some(&c) && go(&p[1..], &t[1..]),
        }
    }
    go(&p, &t)
}

impl Scan {
    /// `full` (workspace-relative, folded) is at or under `p`, or a glob in
    /// it covers `p`.
    fn at_or_under(&self, full: &str) -> bool {
        let parts: Vec<&str> = full.split('/').filter(|c| !c.is_empty()).collect();
        if parts.is_empty() {
            return false; // the root: a documented limit
        }
        let globbed = parts.iter().any(|c| c.contains(['*', '?', '[']));
        let n = parts.len().min(self.p_parts.len());
        if !globbed && parts.len() < self.p_parts.len() {
            return false;
        }
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
            r"\$(?:\{[#!]?([A-Za-z_][A-Za-z0-9_]*)[^}]*\}|([A-Za-z_][A-Za-z0-9_]*)|([0-9@*]))",
        );
        let sub = re(&SUB, r"\$\(([^()]*(?:\([^()]*\)[^()]*)*)\)");
        // command substitutions: a reader's output is opaque content
        let w = sub.replace_all(word, |c: &regex::Captures| {
            let text = &c[1];
            match parse(text, 1) {
                Some(cmds)
                    if !cmds.is_empty()
                        && cmds.iter().all(|cmd| {
                            command_name(&cmd.words).is_some_and(|(n, _)| {
                                is_reader(&n)
                                    || (n == "git"
                                        && cmd.words.iter().any(|a| {
                                            matches!(
                                                a.as_str(),
                                                "rev-parse"
                                                    | "describe"
                                                    | "log"
                                                    | "show"
                                                    | "rev-list"
                                            )
                                        }))
                                    || (n == "sed" && !cmd.words.iter().any(|a| sed_in_place(a)))
                            })
                        }) =>
                {
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
            let name = c.get(1).or(c.get(2)).map(|m| m.as_str());
            let Some(name) = name else {
                return UNKNOWN.to_string(); // $1, $@, $*
            };
            if let Some(v) = self.vars.get(name) {
                return match v {
                    Var::Tainted => TAINT.to_string(),
                    Var::Unknown => UNKNOWN.to_string(),
                    Var::Clean(s) => s.clone(),
                };
            }
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
                "HOME" if hay[c.get(0).unwrap().end()..].starts_with("/.") => OUTSIDE.to_string(),
                _ => UNKNOWN.to_string(),
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
        if s.contains(UNKNOWN) || s.split('/').any(|c| c == "..") && !s.starts_with(ROOT) {
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
        let s = self.normalize(value);
        let v = if s.contains(TAINT) || self.seg_hits_value(&s) {
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

    /// An assigned value names `p` (relative values resolve from the cwds).
    fn seg_hits_value(&self, s: &str) -> bool {
        if s.contains(UNKNOWN) {
            return false;
        }
        self.seg_hits(s)
    }

    /// Any cwd at, under or above `p` (the root aside), or unknown.
    fn in_p(&self) -> bool {
        self.cwds.iter().any(|c| match c {
            Cwd::Path(b) if !b.is_empty() => {
                self.at_or_under(b) || self.p.starts_with(&format!("{b}/"))
            }
            Cwd::Unknown => true,
            _ => false,
        })
    }

    fn code_hits(&self, code: &str) -> bool {
        let n = self.normalize(code);
        n.contains(TAINT) || n.contains(UNKNOWN) || self.aligned(&n) || self.code_al.is_match(&n)
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
                self.assign(k.trim_end_matches('+'), v);
            }
            if w.first().map(String::as_str) == Some("for") && w.len() > 1 {
                let rest = w.iter().skip(3).cloned().collect::<Vec<_>>().join(" ");
                let name = w[1].clone();
                if rest.is_empty() {
                    self.vars.insert(name, Var::Unknown);
                } else {
                    self.assign(&name, &rest);
                }
                continue;
            }
            let Some((name, first)) = command_name(w) else {
                continue;
            };
            let args = &w[first.min(w.len())..];
            match name.as_str() {
                "export" | "declare" | "local" | "readonly" | "typeset" => {
                    for a in args.iter().filter(|a| re_assign().is_match(a)) {
                        let (k, v) = a.split_once('=').unwrap();
                        self.assign(k.trim_end_matches('+'), v);
                    }
                    continue;
                }
                "read" | "mapfile" | "readarray" => {
                    for a in args.iter().filter(|a| !a.starts_with('-')) {
                        self.vars.insert(a.clone(), Var::Unknown);
                    }
                    continue;
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
            if SHELLS.contains(&name.as_str()) {
                if let Some(k) = args
                    .iter()
                    .position(|a| a.starts_with('-') && a.contains('c'))
                {
                    match args.get(k + 1).and_then(|s| parse(s, depth + 1)) {
                        Some(inner) => hit |= self.cmds(&inner, depth + 1),
                        None => hit = true,
                    }
                }
                for body in &cmd.heredocs {
                    match parse(body, depth + 1) {
                        Some(inner) => hit |= self.cmds(&inner, depth + 1),
                        None => hit = true,
                    }
                }
                continue;
            }
            if name == "eval" {
                match parse(&args.join(" "), depth + 1) {
                    Some(inner) => hit |= self.cmds(&inner, depth + 1),
                    None => hit = true,
                }
                continue;
            }
            if INTERPRETERS.contains(&name.as_str()) {
                let in_place = name == "perl" && args.iter().any(|a| short_flag(a, 'i'));
                let inline = args
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| {
                        matches!(
                            a.as_str(),
                            "-c" | "-e" | "-E" | "--eval" | "-Command" | "-command"
                        ) || (a.starts_with('-')
                            && !a.starts_with("--")
                            && (a.contains('e') || a.ends_with('c')))
                    })
                    .filter_map(|(k, _)| args.get(k + 1));
                for code in inline {
                    if self.code_hits(code) {
                        hit = true;
                    }
                }
                for body in &cmd.heredocs {
                    if self.code_hits(body) {
                        hit = true;
                    }
                }
                if in_place && (self.in_p() || args.iter().any(|a| self.word_hits(a))) {
                    hit = true;
                }
                continue;
            }
            if name == "xargs" {
                // the writer's arguments come from stdin
                if args
                    .iter()
                    .find(|a| !a.starts_with('-'))
                    .and_then(|a| command_name(std::slice::from_ref(a)))
                    .is_some_and(|(n, _)| {
                        is_writer(&n) || SHELLS.contains(&n.as_str()) || n == "sed"
                    })
                {
                    hit = true;
                }
                continue;
            }
            let writes = is_writer(&name)
                || (name == "sed" && args.iter().any(|a| sed_in_place(a)))
                || (name == "find"
                    && args.iter().any(|a| {
                        matches!(
                            a.as_str(),
                            "-delete"
                                | "-exec"
                                | "-execdir"
                                | "-ok"
                                | "-okdir"
                                | "-fprint"
                                | "-fprintf"
                                | "-fls"
                        )
                    }));
            if writes && (self.in_p() || args.iter().any(|a| self.word_hits(a))) {
                hit = true;
            }
        }
        hit
    }
}

fn short_flag(a: &str, f: char) -> bool {
    a.starts_with('-') && !a.starts_with("--") && a[1..].contains(f)
}

fn sed_in_place(a: &str) -> bool {
    a.starts_with("--in-place") || short_flag(a, 'i')
}

/// sieve#41: may `script`, run in `wd` with `env` (workflow, job, then step
/// variables, in order), write at, under or above the checkout path `p`?
pub fn may_write(script: &str, env: &[(String, String)], wd: &str, p: &str) -> bool {
    let p = p.to_lowercase();
    let al = Regex::new(&format!(
        r"(^|[/=:,\s{ROOT}{OUTSIDE}]){}(/|$|[\s,;:])",
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
        assert!(!w(
            "# RETRY, POSIX-sh SAFE (gale#289).\ncurl -sSf https://sh.rustup.rs -o /tmp/r.sh"
        ));
        assert!(!w("REV=\"$(grep -v '^#' \"${GITHUB_WORKSPACE}/gale/.github/pin\" | tr -d '[:space:]')\"\ngit -C ws/zephyr fetch --depth 1 origin \"$REV\""));
        assert!(!w(
            "west init -m https://github.com/x/zephyr.git --mr gale/sem-replacement ws"
        ));
        assert!(!may_write(
            "west build -s \"$GALE_ROOT/benches\" -d /tmp/b",
            &[("GALE_ROOT".into(), "${{ github.workspace }}/gale".into())],
            "",
            "gale"
        ));
        assert!(!w(
            "git clone https://github.com/pulseengine/gale.git /tmp/g"
        ));
        assert!(!w(
            "rm -rf /opt/toolchains/zephyr-sdk-* 2>/dev/null || true"
        ));
        assert!(!w("cd /mnt/ws && rm -rf build"));
        assert!(!w("echo x >> \"$GITHUB_PATH\""));
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
        ] {
            assert!(w(s), "missed: {s:?}");
        }
        // a working directory in, or above, the path
        assert!(may_write("git checkout other", &[], "gale", "gale"));
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
}
