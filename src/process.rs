//! Running other programs without ever hanging on them.
//!
//! Every tool ohjaa asks something of (toimija, git, gh, the drafter) can hang.
//! A hang must end as a reported failure, never as a silent wait, so every
//! child runs under a deadline and is killed when it passes.

use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

pub struct Output {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    /// The last non-empty stderr line, or the exit status when there is none.
    pub fn failure_detail(&self) -> String {
        let stderr = String::from_utf8_lossy(&self.stderr);
        match stderr.trim().lines().last().map(str::trim) {
            Some(line) if !line.is_empty() => line.to_owned(),
            _ => format!("exited {}", self.status),
        }
    }
}

/// Run `command` with `input` on stdin, killing it after `timeout`.
pub fn run(command: &mut Command, input: Option<&[u8]>, timeout: Duration) -> Result<Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot run {program}"))?;

    // Feed stdin and drain both pipes on their own threads, so a child that
    // fills a pipe buffer can never deadlock against us.
    let writer = input.map(|bytes| {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let bytes = bytes.to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        })
    });
    let drain = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = pipe.read_to_end(&mut buffer);
            buffer
        })
    };
    let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("{program} did not finish within {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_and_input_pass_through() {
        let out = run(
            Command::new("sh").args(["-c", "cat; echo err >&2"]),
            Some(b"hello"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout_text(), "hello");
        assert_eq!(out.failure_detail(), "err");
    }

    #[test]
    fn a_hang_is_killed_and_reported() {
        let started = Instant::now();
        let error = run(
            Command::new("sh").args(["-c", "sleep 30"]),
            None,
            Duration::from_millis(200),
        )
        .err()
        .expect("a hang must fail");
        assert!(error.to_string().contains("did not finish"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
