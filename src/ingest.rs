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
use std::path::{Path, PathBuf};
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
    /// `(path id, lines)` of every file that already existed when the window
    /// (`--since`, `--max-commits`, a rev range) opens, across the main repo and
    /// its submodules. Empty when every repo's walk starts at its root commit.
    pub baseline: Vec<(u32, u32)>,
    /// Folder paths of the submodules merged into this history.
    pub submodules: Vec<String>,
}

/// Options controlling the git walk.
#[derive(Clone)]
pub struct IngestOptions {
    /// Follow only the first parent (linear mainline). Recommended.
    pub first_parent: bool,
    /// Optional `--since` value passed to git (e.g. "2020-01-01").
    pub since: Option<String>,
    /// Optional `--until` value.
    pub until: Option<String>,
    /// Keep only the most recent N commits of the merged pool. 0 = unlimited.
    pub max_commits: usize,
    /// Branch / revision to walk (default HEAD).
    pub rev: String,
    /// Levels of git submodules whose history is merged in: 0 = none, 1 = the
    /// repository's own submodules (default), 2 = also their submodules, ...
    pub submodule_depth: u32,
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
            submodule_depth: 1,
            submodule_include: Vec::new(),
            submodule_exclude: Vec::new(),
        }
    }
}

const RS: u8 = 0x1e; // record separator: begins each commit header line
const US: u8 = 0x1f; // unit separator: between header fields

/// One repository whose history feeds the shared commit pool: the main repo
/// (empty prefix) or a checked-out submodule, whose files appear under
/// `prefix/` exactly like any other folder.
#[derive(Clone, Debug)]
pub struct RepoSpec {
    pub dir: PathBuf,
    pub prefix: String,
    /// Revision to walk: `--rev` for the main repo; for a submodule, the commit
    /// its parent pins it to.
    pub rev: String,
}

/// Run git in `dir`; stdout on success.
fn git_out(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    git_out(dir, args).map(|b| String::from_utf8_lossy(&b).trim().to_string())
}

/// Find the main repository plus every eligible, checked-out submodule down to
/// `opts.submodule_depth` levels. Cheap (one `ls-tree` + a few `rev-parse` per
/// repo), so it runs before the cache lookup and its result keys the cache.
pub fn discover(repo: &Path, opts: &IngestOptions, quiet: bool) -> Result<Vec<RepoSpec>> {
    // Ask git rather than checking for `.git`, so subdirectories of a repo,
    // linked worktrees (.git file) and bare repos all work.
    if git_out(repo, &["rev-parse", "--git-dir"]).is_none() {
        bail!("not a git repository: {}", repo.display());
    }
    let mut specs = vec![RepoSpec {
        dir: repo.to_path_buf(),
        prefix: String::new(),
        rev: opts.rev.clone(),
    }];
    let mut depth = vec![0u32];
    let mut i = 0;
    while i < specs.len() {
        if depth[i] < opts.submodule_depth {
            let parent = specs[i].clone();
            for (path, sha) in gitlinks(&parent.dir, &parent.rev) {
                let full = if parent.prefix.is_empty() {
                    path.clone()
                } else {
                    format!("{}/{path}", parent.prefix)
                };
                if !submodule_allowed(&full, opts) {
                    continue;
                }
                let dir = parent.dir.join(&path);
                if !is_checked_out(&dir) {
                    if quiet {
                        continue;
                    }
                    eprintln!(
                        "  ! submodule {full} is not checked out, skipped — run `git submodule update --init{}`",
                        if depth[i] > 0 { " --recursive" } else { "" }
                    );
                    continue;
                }
                let pinned = format!("{sha}^{{commit}}");
                let rev = if git_out(&dir, &["cat-file", "-e", &pinned]).is_some() {
                    sha
                } else {
                    if !quiet {
                        eprintln!(
                            "  ! submodule {full}: pinned commit {} is not fetched — using its checked-out HEAD",
                            &sha[..sha.len().min(9)]
                        );
                    }
                    "HEAD".to_string()
                };
                specs.push(RepoSpec {
                    dir,
                    prefix: full,
                    rev,
                });
                depth.push(depth[i] + 1);
            }
        }
        i += 1;
    }
    if specs.len() > 1 && !quiet {
        let names: Vec<&str> = specs[1..].iter().map(|s| s.prefix.as_str()).collect();
        eprintln!(
            "• submodules: merging the history of {} ({})",
            names.len(),
            names.join(", ")
        );
    }
    Ok(specs)
}

/// `(path, pinned sha)` of every submodule (gitlink) in `rev`'s tree.
fn gitlinks(dir: &Path, rev: &str) -> Vec<(String, String)> {
    let Some(out) = git_out(dir, &["ls-tree", "-r", "-z", "--full-tree", rev]) else {
        return Vec::new();
    };
    out.split(|&b| b == 0)
        .filter_map(|entry| {
            // "<mode> <type> <sha>\t<path>"
            let tab = entry.iter().position(|&b| b == b'\t')?;
            let mut meta = entry[..tab].split(|&b| b == b' ');
            if meta.next()? != b"160000" {
                return None;
            }
            let sha = String::from_utf8_lossy(meta.nth(1)?).into_owned();
            let path = String::from_utf8_lossy(&entry[tab + 1..]).into_owned();
            Some((path, sha))
        })
        .collect()
}

/// A submodule directory is usable only when it is its own work tree (an
/// uninitialized one is an empty folder that git resolves to the parent repo).
fn is_checked_out(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    let Some(top) = git_text(dir, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    match (
        std::fs::canonicalize(PathBuf::from(top)),
        std::fs::canonicalize(dir),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Submodule include/exclude by name, path, or substring (case-insensitive).
fn submodule_allowed(path: &str, opts: &IngestOptions) -> bool {
    let p = path.to_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    let matches = |pat: &String| {
        let pat = pat.to_lowercase();
        p == pat || name == pat || p.contains(&pat)
    };
    if opts.submodule_exclude.iter().any(matches) {
        return false;
    }
    opts.submodule_include.is_empty() || opts.submodule_include.iter().any(matches)
}

/// Ingest the main repository and its submodules (from [`discover`]) into one
/// history: every repo is walked separately, the commits are merged into a
/// single timeline by commit time, and `--since` / `--max-commits` apply to that
/// shared pool. Each repo that starts mid-window contributes its state at that
/// point to `History::baseline`.
pub fn ingest(specs: &[RepoSpec], opts: &IngestOptions) -> Result<History> {
    let mut parser = Parser::new();
    let mut runs: Vec<Vec<Commit>> = Vec::with_capacity(specs.len());
    for (i, spec) in specs.iter().enumerate() {
        parser.set_repo(&spec.prefix);
        match walk(spec, opts, &mut parser) {
            Ok(()) => runs.push(std::mem::take(&mut parser.commits)),
            // The main repo must work; a broken submodule only costs its tiles.
            Err(e) if i > 0 => {
                eprintln!("  ! submodule {}: {e:#} — skipped", spec.prefix);
                parser.commits.clear();
                runs.push(Vec::new());
            }
            Err(e) => return Err(e),
        }
    }

    // Last commit each repo walked (fallback state if none of them survive).
    let last_walked: Vec<Option<String>> = runs
        .iter()
        .map(|r| r.last().map(|c| c.hash.clone()))
        .collect();
    let mut pool = merge_by_time(runs);
    if opts.max_commits > 0 && pool.len() > opts.max_commits {
        pool.drain(..pool.len() - opts.max_commits);
    }

    let mut first_kept: Vec<Option<String>> = vec![None; specs.len()];
    for (r, c) in &pool {
        if first_kept[*r].is_none() {
            first_kept[*r] = Some(c.hash.clone());
        }
    }
    let mut baseline = Vec::new();
    for (r, spec) in specs.iter().enumerate() {
        let state_at = match (&first_kept[r], &last_walked[r]) {
            // Starts mid-window: everything before its first kept commit.
            (Some(first), _) => git_text(
                &spec.dir,
                &["rev-parse", "--verify", "-q", &format!("{first}^")],
            ),
            // No commits in the window at all: it stands as of its last one.
            (None, Some(last)) => Some(last.clone()),
            (None, None) => opts.since.as_ref().and_then(|since| {
                let mut args = vec!["rev-list", "-1"];
                if opts.first_parent {
                    args.push("--first-parent");
                }
                let before = format!("--before={since}");
                args.push(&before);
                args.push(&spec.rev);
                git_text(&spec.dir, &args).filter(|s| !s.is_empty())
            }),
        };
        if let Some(commit) = state_at {
            parser.set_repo(&spec.prefix);
            baseline.extend(read_tree(&spec.dir, &commit, &mut parser)?);
        }
    }

    Ok(History {
        paths: parser.paths.into_names(),
        authors: parser.author_meta,
        commits: pool.into_iter().map(|(_, c)| c).collect(),
        baseline,
        submodules: specs[1..].iter().map(|s| s.prefix.clone()).collect(),
    })
}

/// Stream one repository's `git log` into the parser.
fn walk(spec: &RepoSpec, opts: &IngestOptions, parser: &mut Parser) -> Result<()> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(&spec.dir)
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
    // The pool's last N commits hold at most the last N of each repo, so this
    // is safe as a per-repo pre-filter; the exact cut happens on the pool.
    if opts.max_commits > 0 {
        cmd.arg(format!("--max-count={}", opts.max_commits));
    }
    cmd.arg(&spec.rev);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

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
    Ok(())
}

/// Merge per-repo commit runs into one timeline by commit time. A k-way merge,
/// so each repo keeps its own (topological) order even where its commit dates
/// are skewed. Ties go to the lower repo index (main repo first).
fn merge_by_time(runs: Vec<Vec<Commit>>) -> Vec<(usize, Commit)> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let total = runs.iter().map(Vec::len).sum();
    let mut iters: Vec<std::iter::Peekable<std::vec::IntoIter<Commit>>> =
        runs.into_iter().map(|r| r.into_iter().peekable()).collect();
    let mut heap = BinaryHeap::new();
    for (r, it) in iters.iter_mut().enumerate() {
        if let Some(c) = it.peek() {
            heap.push(Reverse((c.time, r)));
        }
    }
    let mut out = Vec::with_capacity(total);
    while let Some(Reverse((_, r))) = heap.pop() {
        let c = iters[r].next().expect("peeked commit");
        out.push((r, c));
        if let Some(n) = iters[r].peek() {
            heap.push(Reverse((n.time, r)));
        }
    }
    out
}

/// `(path id, lines)` of every file in `commit`'s tree.
fn read_tree(dir: &Path, commit: &str, parser: &mut Parser) -> Result<Vec<(u32, u32)>> {
    Ok(feed_tree(&tree_dump(dir, commit)?, parser))
}

/// Raw `--raw --numstat` listing of `commit`'s whole tree, as a diff against
/// the empty tree. This is the expensive part (git reads every blob to count its
/// lines), and it needs no parser, so several repos can be dumped in parallel.
fn tree_dump(dir: &Path, commit: &str) -> Result<Vec<u8>> {
    let empty_tree = git_text(dir, &["hash-object", "-t", "tree", "--stdin"])
        .context("git hash-object (empty tree) failed")?;
    git_out(
        dir,
        &[
            "diff-tree",
            "-r",
            "--no-renames",
            "--raw",
            "--numstat",
            &empty_tree,
            commit,
        ],
    )
    .with_context(|| format!("git diff-tree (tree state of {commit}) failed"))
}

/// Parse a [`tree_dump`] through the very same parser as the walk, so binary
/// files, gitlinks and line counts behave exactly the same.
fn feed_tree(dump: &[u8], parser: &mut Parser) -> Vec<(u32, u32)> {
    parser.begin_baseline();
    for line in dump.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        parser.feed(line);
    }
    parser.flush_commit();
    let pseudo = parser.commits.pop().expect("baseline pseudo-commit");
    pseudo
        .changes
        .into_iter()
        .filter(|ch| ch.kind != ChangeKind::Deleted)
        .map(|ch| (ch.path, ch.added))
        .collect()
}

/// The repository as the video shows it at `commit`, without walking any
/// history: the commit itself (for the HUD and its highlights) plus, as
/// `baseline`, the main repo's tree at `commit` and every submodule of the
/// rendered revision (`opts.rev`) at its last commit no newer than `commit` —
/// the same by-date rule as the merged timeline. Cost scales with the size of
/// the tree, not the length of the history.
pub fn snapshot_at(repo: &Path, opts: &IngestOptions, commit: &str) -> Result<History> {
    let one = IngestOptions {
        rev: commit.to_string(),
        since: None,
        until: None,
        max_commits: 1,
        ..opts.clone()
    };
    let mut specs = discover(repo, opts, false)?;
    specs[0].rev = commit.to_string();
    let mut parser = Parser::new();
    walk(&specs[0], &one, &mut parser)?;
    let target = parser
        .commits
        .pop()
        .with_context(|| format!("commit {commit} not found"))?;

    // Main repo at the target; each submodule as of the target's date (one
    // that had no commits yet by then is simply absent).
    let before = format!("--before={}", target.time);
    let mut repos = vec![specs[0].clone()];
    for spec in &specs[1..] {
        let mut args = vec!["rev-list", "-1"];
        if opts.first_parent {
            args.push("--first-parent");
        }
        args.push(&before);
        args.push(&spec.rev);
        if let Some(rev) = git_text(&spec.dir, &args).filter(|s| !s.is_empty()) {
            repos.push(RepoSpec {
                rev,
                ..spec.clone()
            });
        }
    }

    // Dump every repo's tree concurrently (bounded by the core count).
    let par = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .max(1);
    let mut dumps: Vec<Result<Vec<u8>>> = Vec::with_capacity(repos.len());
    for chunk in repos.chunks(par) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|spec| scope.spawn(move || tree_dump(&spec.dir, &spec.rev)))
                .collect();
            for h in handles {
                dumps.push(
                    h.join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("tree reader panicked"))),
                );
            }
        });
    }

    let mut baseline = Vec::new();
    for (i, (spec, dump)) in repos.iter().zip(dumps).enumerate() {
        match dump {
            Ok(d) => {
                parser.set_repo(&spec.prefix);
                baseline.extend(feed_tree(&d, &mut parser));
            }
            Err(e) if i > 0 => eprintln!("  ! submodule {}: {e:#} — skipped", spec.prefix),
            Err(e) => return Err(e),
        }
    }
    Ok(History {
        paths: parser.paths.into_names(),
        authors: parser.author_meta,
        commits: vec![target],
        baseline,
        submodules: specs[1..].iter().map(|s| s.prefix.clone()).collect(),
    })
}

/// `(position, total)` of `commit` in the merged commit pool of `opts.rev`,
/// counted without walking any history (`git rev-list --count` per repo), so a
/// snapshot's "commit N/M" matches the video's: the main repo's commits up to
/// `commit` plus each submodule's commits no newer than `time`.
pub fn pool_position(repo: &Path, opts: &IngestOptions, commit: &str, time: i64) -> (usize, usize) {
    let count = |dir: &Path, rev: &str, before: Option<i64>| -> usize {
        let before = before.map(|t| format!("--before={t}"));
        let mut args = vec!["rev-list", "--count"];
        if opts.first_parent {
            args.push("--first-parent");
        }
        if let Some(b) = &before {
            args.push(b);
        }
        args.push(rev);
        git_text(dir, &args)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    };
    let specs = discover(repo, opts, true).unwrap_or_default();
    let (mut pos, mut total) = (0, 0);
    for (i, spec) in specs.iter().enumerate() {
        total += count(&spec.dir, &spec.rev, None);
        pos += if i == 0 {
            count(&spec.dir, commit, None)
        } else {
            count(&spec.dir, &spec.rev, Some(time))
        };
    }
    let total = total.max(1);
    (pos.clamp(1, total) - 1, total)
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
    /// Folder the current repo's files live under ("" for the main repo).
    prefix: String,
    /// Prepended to commit subjects of submodule commits ("[libs/ui] ").
    tag: String,
}

impl Parser {
    fn new() -> Self {
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
            prefix: String::new(),
            tag: String::new(),
        }
    }

    /// Switch to another repo of the pool: its paths go under `prefix/`.
    fn set_repo(&mut self, prefix: &str) {
        self.flush_commit();
        self.prefix = prefix.to_string();
        self.tag = if prefix.is_empty() {
            String::new()
        } else {
            format!("[{prefix}] ")
        };
    }

    fn path_id(&mut self, raw: &[u8]) -> u32 {
        let p = String::from_utf8_lossy(raw);
        if self.prefix.is_empty() {
            self.paths.intern(&p)
        } else {
            self.paths.intern(&format!("{}/{p}", self.prefix))
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

    /// Open a pseudo-commit that collects the baseline diff (no author/subject).
    fn begin_baseline(&mut self) {
        self.flush_commit();
        self.cur_hash.clear();
        self.cur_subject.clear();
        self.cur_author = 0;
        self.cur_time = 0;
        self.raw.clear();
        self.numstat.clear();
        self.have_commit = true;
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
        let id = self.path_id(path);
        self.raw.insert(id, (status, is_gitlink));
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
        let id = self.path_id(path);
        self.numstat.push((id, a, d, binary));
    }

    fn flush_commit(&mut self) {
        if !self.have_commit {
            return;
        }
        let mut changes = Vec::with_capacity(self.numstat.len());
        for &(path, added, deleted, binary) in &self.numstat {
            // Gitlinks carry no files; checked-out submodules are walked as
            // repos of their own (see `ingest`), everything else is skipped.
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

        // Some commits (merge with no first-parent delta, empty commits) have no
        // changes; keep them anyway so the timeline/committer still appears.
        self.commits.push(Commit {
            hash: std::mem::take(&mut self.cur_hash),
            author: self.cur_author,
            time: self.cur_time,
            subject: format!("{}{}", self.tag, std::mem::take(&mut self.cur_subject)),
            changes,
        });
        self.have_commit = false;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn c(hash: &str, time: i64) -> Commit {
        Commit {
            hash: hash.into(),
            author: 0,
            time,
            subject: String::new(),
            changes: Vec::new(),
        }
    }

    #[test]
    fn pool_merges_by_time_but_keeps_each_repos_order() {
        // Main repo has a skewed date (m2 older than m1); it must stay after m1.
        let main = vec![c("m1", 10), c("m2", 5), c("m3", 40)];
        let sub = vec![c("s1", 1), c("s2", 20), c("s3", 30)];
        let order: Vec<String> = merge_by_time(vec![main, sub])
            .into_iter()
            .map(|(_, c)| c.hash)
            .collect();
        assert_eq!(order, ["s1", "m1", "m2", "s2", "s3", "m3"]);
    }

    #[test]
    fn submodule_filters_match_name_path_or_substring() {
        let opts = IngestOptions {
            submodule_exclude: vec!["Legacy".into()],
            ..IngestOptions::default()
        };
        assert!(submodule_allowed("libs/ui", &opts));
        assert!(!submodule_allowed("libs/legacy-api", &opts));
        let only = IngestOptions {
            submodule_include: vec!["ui".into()],
            ..IngestOptions::default()
        };
        assert!(submodule_allowed("libs/ui", &only));
        assert!(!submodule_allowed("libs/core", &only));
    }
}
