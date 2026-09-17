//! ffmpeg encoding pipeline. Spawns the system ffmpeg once, feeds it raw rgb24
//! frames in order over stdin, drains stderr on a thread (deadlock-safe), and
//! reports a helpful error if ffmpeg is missing or exits nonzero.

use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail};

use crate::config::{Codec, Config, Quality};

pub struct Encoder {
    child: Child,
    writer: Option<BufWriter<ChildStdin>>,
    stderr_join: Option<JoinHandle<String>>,
    pub frame_bytes: usize,
}

fn build_args(cfg: &Config, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push("-s".into());
    a.push(format!("{}x{}", cfg.width, cfg.height));
    a.push("-r".into());
    a.push(cfg.fps.to_string());
    a.push("-i".into());
    a.push("-".into());
    a.push("-an".into());

    match cfg.codec {
        Codec::Mp4 => {
            let (crf, preset) = match cfg.quality {
                Quality::Draft => ("28", "veryfast"),
                Quality::Balanced => ("20", "medium"),
                Quality::High => ("16", "slow"),
            };
            for s in [
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-crf",
                crf,
                "-preset",
                preset,
                "-movflags",
                "+faststart",
            ] {
                a.push(s.to_string());
            }
        }
        Codec::Webm => {
            let (crf, cpu) = match cfg.quality {
                Quality::Draft => ("34", "5"),
                Quality::Balanced => ("31", "2"),
                Quality::High => ("28", "1"),
            };
            for s in [
                "-c:v",
                "libvpx-vp9",
                "-pix_fmt",
                "yuv420p",
                "-crf",
                crf,
                "-b:v",
                "0",
                "-row-mt",
                "1",
                "-deadline",
                "good",
                "-cpu-used",
                cpu,
            ] {
                a.push(s.to_string());
            }
        }
    }
    a.push(out.to_string_lossy().to_string());
    a
}

impl Encoder {
    pub fn new(cfg: &Config) -> Result<Self> {
        if let Some(parent) = cfg.out.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).ok();
        }
        let args = build_args(cfg, &cfg.out);
        let mut child = match Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("`ffmpeg` not found on PATH — install ffmpeg to encode video")
            }
            Err(e) => return Err(e).context("spawning ffmpeg"),
        };

        let stdin = child.stdin.take().expect("piped stdin");
        let mut stderr = child.stderr.take().expect("piped stderr");
        let stderr_join = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s);
            s
        });

        Ok(Encoder {
            child,
            writer: Some(BufWriter::with_capacity(1 << 20, stdin)),
            stderr_join: Some(stderr_join),
            frame_bytes: (cfg.width * cfg.height * 3) as usize,
        })
    }

    /// Write one frame (must be called in frame order).
    pub fn write_frame(&mut self, buf: &[u8]) -> Result<()> {
        let w = self.writer.as_mut().expect("writer");
        match w.write_all(buf) {
            Ok(()) => Ok(()),
            Err(e) => {
                // ffmpeg likely died; surface its stderr.
                let tail = self.take_stderr_tail();
                bail!("failed writing frame to ffmpeg ({e}):\n{tail}");
            }
        }
    }

    fn take_stderr_tail(&mut self) -> String {
        // Best-effort: if the drain thread finished, get its output.
        if let Some(j) = self.stderr_join.take() {
            j.join().unwrap_or_default()
        } else {
            String::new()
        }
    }

    /// Flush, close stdin, wait for ffmpeg to finish, and verify success.
    pub fn finish(mut self) -> Result<()> {
        if let Some(mut w) = self.writer.take() {
            w.flush().context("flushing ffmpeg stdin")?;
            // Drop closes the pipe -> EOF.
            drop(w);
        }
        let status = self.child.wait().context("waiting for ffmpeg")?;
        let tail = self
            .stderr_join
            .take()
            .map(|j| j.join().unwrap_or_default())
            .unwrap_or_default();
        if !status.success() {
            bail!(
                "ffmpeg exited with {}:\n{}",
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                tail.trim()
            );
        }
        Ok(())
    }
}

/// Verify ffmpeg exists and supports the chosen encoder; returns a friendly error.
pub fn check_ffmpeg(cfg: &Config) -> Result<()> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let out = match out {
        Ok(o) => o,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            bail!("`ffmpeg` not found on PATH — install ffmpeg (e.g. `apt install ffmpeg`)")
        }
        Err(e) => return Err(e).context("running ffmpeg -encoders"),
    };
    let enc = String::from_utf8_lossy(&out.stdout);
    let needed = match cfg.codec {
        Codec::Mp4 => "libx264",
        Codec::Webm => "libvpx-vp9",
    };
    if !enc.contains(needed) {
        bail!(
            "ffmpeg is installed but lacks the '{needed}' encoder needed for the selected format"
        );
    }
    Ok(())
}
