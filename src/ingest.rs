//! Git history ingestion by streaming `git log --raw --numstat`.
//!
//! Rationale: shelling out to the `git` CLI keeps the whole crate pure-Rust
//! (no libgit2/cmake native build), streams the entire history in one pass with
//! minimal memory, and gives us line-count deltas (the natural metric for a
//! "minimap lines" visualization) plus free binary-file detection (binary files
//! show as `-` in numstat). We combine `--raw` (for A/M/D status) with
//! `--numstat` (for counts + binary flag); both blocks list files in the same
//! order per commit and we key them by path.

use anyhow::{Context, Result, bail};
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::intern::Interner;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

/// One file touched by a commit (already filtered: binary files are dropped
/// before this is constructed).
#[derive(Clone, Copy, Debug)]
pub struct FileDelta {
    pub path: u32,
    pub added: u32,
    pub deleted: u32,
    pub kind: ChangeKind,
}

#[derive(Clone, Debug)]
pub struct Author {
    pub name: String,
    pub email: String,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub hash: String,
    pub author: u32,
    pub time: i64,
    pub subject: String,
    pub changes: Vec<FileDelta>,
}

pub struct History {
    /// id -> path string.
    pub paths: Vec<String>,
    /// id -> author.
    pub authors: Vec<Author>,
    /// Commits in chronological order (oldest first).
    pub commits: Vec<Commit>,
}

/// Options controlling the git walk.
pub struct IngestOptions {
    /// Follow only the first parent (linear mainline). Recommended.
    pub first_parent: bool,
    /// Optional `--since` value passed to git (e.g. "2020-01-01").
    pub since: Option<String>,
    /// Optional `--until` value.
    pub until: Option<String>,
    /// Optional max number of commits (git `--max-count`). 0 = unlimited.
    pub max_commits: usize,
    /// Branch / revision to walk (default HEAD).
    pub rev: String,
    /// Include git submodules (gitlinks) as tiles. Default true.
    pub submodules: bool,
    /// If non-empty, only submodules matching one of these are included.
    pub submodule_include: Vec<String>,
    /// Submodules matching any of these are excluded.
    pub submodule_exclude: Vec<String>,
}

impl Default for IngestOptions {
    fn default() -> Self {
        IngestOptions {
            first_parent: true,
            since: None,
            until: None,
            max_commits: 0,
            rev: "HEAD".to_string(),
            submodules: true,
            submodule_include: Vec::new(),
            submodule_exclude: Vec::new(),
        }
    }
}

/// Nominal line count assigned to a submodule tile (submodules have no lines of
/// their own; this keeps them visible as a small, stable tile).
const SUBMODULE_NOMINAL: u32 = 25;

const RS: u8 = 0x1e; // record separator: begins each commit header line
const US: u8 = 0x1f; // unit separator: between header fields

/// Ingest the full history of the repository at `repo_path`.
pub fn ingest(repo_path: &Path, opts: &IngestOptions) -> Result<History> {
    // Confirm it's a git repo up front for a friendly error. Ask git rather than
    // checking for `.git` directly, so subdirectories of a repo, linked worktrees
    // (.git file) and bare repos all work.
    let is_repo = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !is_repo {
        bail!("not a git repository: {}", repo_path.display());
    }

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo_path)
        .arg("-c")
        .arg("core.quotepath=false")
        .arg("log")
        .arg("--reverse")
        .arg("--no-renames")
        .arg("--raw")
        .arg("--numstat")
        .arg("--pretty=format:%x1e%H%x1f%an%x1f%ae%x1f%at%x1f%s");

    if opts.first_parent {
        cmd.arg("--first-parent").arg("--diff-merges=first-parent");
    }
    if let Some(since) = &opts.since {
        cmd.arg(format!("--since={since}"));
    }
    if let Some(until) = &opts.until {
        cmd.arg(format!("--until={until}"));
    }
    if opts.max_commits > 0 {
        cmd.arg(format!("--max-count={}", opts.max_commits));
    }
    cmd.arg(&opts.rev);

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .context("failed to spawn `git` — is git installed and on PATH?")?;

    let stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    // Drain stderr on a thread so a large diagnostic can't deadlock the pipe.
    let err_handle = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });

    let mut parser = Parser::new(opts);
    let mut reader = BufReader::with_capacity(1 << 20, stdout);
    let mut line: Vec<u8> = Vec::with_capacity(256);
    loop {
        line.clear();
        let n = read_until(&mut reader, b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        // Strip trailing \n and \r.
        while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
            line.pop();
        }
        parser.feed(&line);
    }
    parser.flush_commit();

    let status = child.wait().context("waiting for git")?;
    if !status.success() {
        let err = err_handle.join().unwrap_or_default();
        bail!(
            "git log failed ({}):\n{}",
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
            err.trim()
        );
    }
    let _ = err_handle.join();

    Ok(parser.finish())
}

/// Read from `r` into `buf` until (and including) `delim`, byte-oriented so that
/// non-UTF-8 paths don't panic. Returns bytes read.
fn read_until<R: std::io::BufRead>(r: &mut R, delim: u8, buf: &mut Vec<u8>) -> Result<usize> {
    let mut total = 0;
    loop {
        let available = match r.fill_buf() {
            Ok(b) => b,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        };
        if available.is_empty() {
            return Ok(total);
        }
        match available.iter().position(|&b| b == delim) {
            Some(i) => {
                buf.extend_from_slice(&available[..=i]);
                let consumed = i + 1;
                r.consume(consumed);
                total += consumed;
                return Ok(total);
            }
            None => {
                buf.extend_from_slice(available);
                let consumed = available.len();
                r.consume(consumed);
                total += consumed;
            }
        }
    }
}

struct Parser {
    paths: Interner,
    authors: Interner,
    author_meta: Vec<Author>,
    commits: Vec<Commit>,

    // Per-commit accumulation.
    cur_hash: String,
    cur_author: u32,
    cur_time: i64,
    cur_subject: String,
    // path id -> (status letter, is_gitlink) from the --raw block.
    raw: std::collections::HashMap<u32, (u8, bool)>,
    // numstat entries: (path id, added, deleted, binary).
    numstat: Vec<(u32, u32, u32, bool)>,
    have_commit: bool,
    // submodule handling
    submodules: bool,
    sub_include: Vec<String>,
    sub_exclude: Vec<String>,
}

impl Parser {
    fn new(opts: &IngestOptions) -> Self {
        Parser {
            paths: Interner::new(),
            authors: Interner::new(),
            author_meta: Vec::new(),
            commits: Vec::new(),
            cur_hash: String::new(),
            cur_author: 0,
            cur_time: 0,
            cur_subject: String::new(),
            raw: std::collections::HashMap::new(),
            numstat: Vec::new(),
            have_commit: false,
            submodules: opts.submodules,
            sub_include: opts
                .submodule_include
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            sub_exclude: opts
                .submodule_exclude
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
        }
    }

    fn feed(&mut self, line: &[u8]) {
        if line.is_empty() {
            return;
        }
        if line[0] == RS {
            self.flush_commit();
            self.start_commit(&line[1..]);
        } else if line[0] == b':' {
            self.parse_raw(line);
        } else {
            self.parse_numstat(line);
        }
    }

    fn start_commit(&mut self, rest: &[u8]) {
        // fields: hash US name US email US time US subject
        let mut fields = rest.split(|&b| b == US);
        let hash = fields.next().unwrap_or(b"");
        let name = fields.next().unwrap_or(b"");
        let email = fields.next().unwrap_or(b"");
        let time = fields.next().unwrap_or(b"");
        // Subject may (rarely) contain US; rejoin the remainder.
        let subj_parts: Vec<&[u8]> = fields.collect();
        let subject = subj_parts.join(&US);

        self.cur_hash = String::from_utf8_lossy(hash).into_owned();
        let name_s = String::from_utf8_lossy(name).into_owned();
        let email_s = String::from_utf8_lossy(email).into_owned();
        // Intern author by "name <email>" identity.
        let key = format!("{name_s} <{email_s}>");
        let id = self.authors.intern(&key);
        if id as usize == self.author_meta.len() {
            self.author_meta.push(Author {
                name: name_s,
                email: email_s,
            });
        }
        self.cur_author = id;
        self.cur_time = parse_i64(time);
        self.cur_subject = String::from_utf8_lossy(&subject).into_owned();
        self.raw.clear();
        self.numstat.clear();
        self.have_commit = true;
    }

    fn parse_raw(&mut self, line: &[u8]) {
        // ":<oldmode> <newmode> <oldsha> <newsha> <STATUS>\t<path>"
        let tab = match line.iter().position(|&b| b == b'\t') {
            Some(i) => i,
            None => return,
        };
        let meta = &line[..tab];
        let path = &line[tab + 1..];
        let tokens: Vec<&[u8]> = meta
            .split(|&b| b == b' ')
            .filter(|s| !s.is_empty())
            .collect();
        // tokens[0] = ":<oldmode>", tokens[1] = newmode, ..., last = status
        let status = tokens
            .last()
            .and_then(|s| s.first().copied())
            .unwrap_or(b'M');
        // Gitlink (submodule) has mode 160000 in old or new position.
        let oldmode = tokens.first().map(|t| t.strip_prefix(b":").unwrap_or(t));
        let newmode = tokens.get(1).copied();
        let is_gitlink = oldmode == Some(b"160000") || newmode == Some(b"160000".as_slice());
        let path_str = String::from_utf8_lossy(path);
        let id = self.paths.intern(&path_str);
        self.raw.insert(id, (status, is_gitlink));
    }

    fn submodule_allowed(&self, path: &str) -> bool {
        let p = path.to_lowercase();
        let name = p.rsplit('/').next().unwrap_or(&p);
        let matches = |pat: &str| p == *pat || name == pat || p.contains(pat);
        if self.sub_exclude.iter().any(|pat| matches(pat)) {
            return false;
        }
        if !self.sub_include.is_empty() && !self.sub_include.iter().any(|pat| matches(pat)) {
            return false;
        }
        true
    }

    fn parse_numstat(&mut self, line: &[u8]) {
        // "<added>\t<deleted>\t<path>"  (added/deleted may be '-' for binary)
        let mut it = line.splitn(3, |&b| b == b'\t');
        let added = it.next().unwrap_or(b"");
        let deleted = it.next().unwrap_or(b"");
        let path = match it.next() {
            Some(p) => p,
            None => return,
        };
        let binary = added == b"-" || deleted == b"-";
        let a = if binary { 0 } else { parse_u32(added) };
        let d = if binary { 0 } else { parse_u32(deleted) };
        let path_str = String::from_utf8_lossy(path);
        let id = self.paths.intern(&path_str);
        self.numstat.push((id, a, d, binary));
    }

    fn flush_commit(&mut self) {
        if !self.have_commit {
            return;
        }
        let mut changes = Vec::with_capacity(self.numstat.len());
        for &(path, added, deleted, binary) in &self.numstat {
            // Submodule (gitlink) entries are handled from the raw block below.
            if let Some((_, true)) = self.raw.get(&path) {
                continue;
            }
            if binary {
                continue; // exclude binary files entirely
            }
            let status = self.raw.get(&path).map(|&(s, _)| s).unwrap_or(b'M');
            let kind = match status {
                b'A' => ChangeKind::Added,
                b'D' => ChangeKind::Deleted,
                _ => ChangeKind::Modified, // M, T, and anything else
            };
            changes.push(FileDelta {
                path,
                added,
                deleted,
                kind,
            });
        }

        // Submodules: emit from the raw block (authoritative for gitlinks).
        if self.submodules {
            // Deterministic order: sort gitlink paths by id.
            let mut subs: Vec<(u32, u8)> = self
                .raw
                .iter()
                .filter(|(_, v)| v.1)
                .map(|(p, v)| (*p, v.0))
                .collect();
            subs.sort_by_key(|&(p, _)| p);
            for (path, status) in subs {
                let name = self.paths.get(path);
                if !self.submodule_allowed(name) {
                    continue;
                }
                match status {
                    b'A' => changes.push(FileDelta {
                        path,
                        added: SUBMODULE_NOMINAL,
                        deleted: 0,
                        kind: ChangeKind::Added,
                    }),
                    b'D' => changes.push(FileDelta {
                        path,
                        added: 0,
                        deleted: 0,
                        kind: ChangeKind::Deleted,
                    }),
                    // Pointer bump: net-zero for an existing submodule, but sizes
                    // a first-seen one to the nominal value (model treats an
                    // unseen Modified as an Add of `added`).
                    _ => changes.push(FileDelta {
                        path,
                        added: SUBMODULE_NOMINAL,
                        deleted: SUBMODULE_NOMINAL,
                        kind: ChangeKind::Modified,
                    }),
                }
            }
        }
        // Some commits (merge with no first-parent delta, empty commits) have no
        // changes; keep them anyway so the timeline/committer still appears.
        self.commits.push(Commit {
            hash: std::mem::take(&mut self.cur_hash),
            author: self.cur_author,
            time: self.cur_time,
            subject: std::mem::take(&mut self.cur_subject),
            changes,
        });
        self.have_commit = false;
    }

    fn finish(self) -> History {
        History {
            paths: self.paths.into_names(),
            authors: self.author_meta,
            commits: self.commits,
        }
    }
}

fn parse_u32(b: &[u8]) -> u32 {
    let mut n: u32 = 0;
    for &c in b {
        if c.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((c - b'0') as u32);
        }
    }
    // Cap at i32::MAX so the model's i32 line-count math can never overflow
    // (no real numstat line count comes anywhere near 2.1 billion).
    n.min(i32::MAX as u32)
}

fn parse_i64(b: &[u8]) -> i64 {
    let mut n: i64 = 0;
    let mut neg = false;
    for (i, &c) in b.iter().enumerate() {
        if i == 0 && c == b'-' {
            neg = true;
            continue;
        }
        if c.is_ascii_digit() {
            n = n.saturating_mul(10).saturating_add((c - b'0') as i64);
        }
    }
    if neg { -n } else { n }
}
