//! On-disk cache of parsed history, so re-renders (different visual options)
//! skip the multi-second `git log` pass. Stored inside the repo's `.git` dir and
//! keyed by the resolved HEAD sha plus the ingest options.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::ingest::{Author, Commit, FileDelta, History, IngestOptions, RepoSpec};

const MAGIC: &[u8; 8] = b"GATLAS\x04\x00";

fn cache_path(repo: &Path) -> PathBuf {
    repo.join(".git").join("gitatlas-history.bin")
}

/// Every repo of the pool (main + submodules) by resolved sha, plus the ingest
/// options: a new commit, a newly initialized or re-pinned submodule, or any
/// option change invalidates the cache.
fn key(specs: &[RepoSpec], opts: &IngestOptions) -> Option<String> {
    let mut repos = String::new();
    for spec in specs {
        let out = Command::new("git")
            .arg("-C")
            .arg(&spec.dir)
            .args(["rev-parse", &spec.rev])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        repos.push_str(&format!("{}={sha};", spec.prefix));
    }
    Some(format!(
        "{repos}|fp={}|since={:?}|until={:?}|max={}|rev={}|subdepth={}|subin={:?}|subex={:?}",
        opts.first_parent,
        opts.since,
        opts.until,
        opts.max_commits,
        opts.rev,
        opts.submodule_depth,
        opts.submodule_include,
        opts.submodule_exclude
    ))
}

pub fn load(specs: &[RepoSpec], opts: &IngestOptions) -> Option<History> {
    let k = key(specs, opts)?;
    let path = cache_path(&specs[0].dir);
    let bytes = std::fs::read(path).ok()?;
    let mut r = Reader { b: &bytes, pos: 0 };
    if r.take(8)? != MAGIC {
        return None;
    }
    let stored_key = r.string()?;
    if stored_key != k {
        return None;
    }
    let np = r.u32()? as usize;
    let mut paths = Vec::with_capacity(np);
    for _ in 0..np {
        paths.push(r.string()?);
    }
    let na = r.u32()? as usize;
    let mut authors = Vec::with_capacity(na);
    for _ in 0..na {
        let name = r.string()?;
        let email = r.string()?;
        authors.push(Author { name, email });
    }
    let nc = r.u32()? as usize;
    let mut commits = Vec::with_capacity(nc);
    for _ in 0..nc {
        let hash = r.string()?;
        let author = r.u32()?;
        let time = r.i64()?;
        let subject = r.string()?;
        let ncg = r.u32()? as usize;
        let mut changes = Vec::with_capacity(ncg);
        for _ in 0..ncg {
            let path = r.u32()?;
            let added = r.u32()?;
            let deleted = r.u32()?;
            let kind = r.u8()?;
            changes.push(FileDelta {
                path,
                added,
                deleted,
                kind: match kind {
                    0 => crate::ingest::ChangeKind::Added,
                    2 => crate::ingest::ChangeKind::Deleted,
                    _ => crate::ingest::ChangeKind::Modified,
                },
            });
        }
        commits.push(Commit {
            hash,
            author,
            time,
            subject,
            changes,
        });
    }
    let nb = r.u32()? as usize;
    let mut baseline = Vec::with_capacity(nb);
    for _ in 0..nb {
        baseline.push((r.u32()?, r.u32()?));
    }
    let ns = r.u32()? as usize;
    let mut submodules = Vec::with_capacity(ns);
    for _ in 0..ns {
        submodules.push(r.string()?);
    }
    Some(History {
        paths,
        authors,
        commits,
        baseline,
        submodules,
    })
}

pub fn save(specs: &[RepoSpec], opts: &IngestOptions, history: &History) -> Result<()> {
    let k = match key(specs, opts) {
        Some(k) => k,
        None => return Ok(()),
    };
    let mut w = Writer {
        b: Vec::with_capacity(1 << 20),
    };
    w.b.extend_from_slice(MAGIC);
    w.string(&k);
    w.u32(history.paths.len() as u32);
    for p in &history.paths {
        w.string(p);
    }
    w.u32(history.authors.len() as u32);
    for a in &history.authors {
        w.string(&a.name);
        w.string(&a.email);
    }
    w.u32(history.commits.len() as u32);
    for c in &history.commits {
        w.string(&c.hash);
        w.u32(c.author);
        w.i64(c.time);
        w.string(&c.subject);
        w.u32(c.changes.len() as u32);
        for ch in &c.changes {
            w.u32(ch.path);
            w.u32(ch.added);
            w.u32(ch.deleted);
            w.u8(match ch.kind {
                crate::ingest::ChangeKind::Added => 0,
                crate::ingest::ChangeKind::Modified => 1,
                crate::ingest::ChangeKind::Deleted => 2,
            });
        }
    }
    w.u32(history.baseline.len() as u32);
    for &(path, lines) in &history.baseline {
        w.u32(path);
        w.u32(lines);
    }
    w.u32(history.submodules.len() as u32);
    for sm in &history.submodules {
        w.string(sm);
    }
    // Write atomically-ish: temp then rename.
    let path = cache_path(&specs[0].dir);
    if let Some(parent) = path.parent()
        && !parent.exists()
    {
        return Ok(()); // no .git dir; skip caching
    }
    let tmp = path.with_extension("bin.tmp");
    {
        let f = std::fs::File::create(&tmp)?;
        let mut bw = std::io::BufWriter::new(f);
        bw.write_all(&w.b)?;
        bw.flush()?;
    }
    std::fs::rename(&tmp, &path).ok();
    Ok(())
}

struct Writer {
    b: Vec<u8>,
}
impl Writer {
    fn u8(&mut self, v: u8) {
        self.b.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.b.extend_from_slice(&v.to_le_bytes());
    }
    fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.b.extend_from_slice(s.as_bytes());
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.pos + n > self.b.len() {
            return None;
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i64(&mut self) -> Option<i64> {
        let b = self.take(8)?;
        Some(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let b = self.take(n)?;
        Some(String::from_utf8_lossy(b).into_owned())
    }
}
