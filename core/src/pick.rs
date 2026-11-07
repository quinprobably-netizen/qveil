use anyhow::{Context, Result};
use std::io::{BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// Lets the user choose a line, using fzf if it's installed. None = cancelled.
pub fn pick(lines: &[String]) -> Result<Option<usize>> {
    if lines.is_empty() {
        return Ok(None);
    }
    if on_path("fzf") {
        fzf(lines)
    } else {
        numbered(lines)
    }
}

fn fzf(lines: &[String]) -> Result<Option<usize>> {
    let mut child = Command::new("fzf")
        .args([
            "--delimiter=\t",
            "--with-nth=2..",
            "--no-sort",
            "--layout=reverse",
            "--height=70%",
            "--prompt=qveil> ",
            "--header=enter: play   esc: quit",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting fzf")?;
    {
        let mut stdin = child.stdin.take().context("fzf stdin")?;
        for (i, l) in lines.iter().enumerate() {
            // fzf may exit before reading everything; that's a cancel, not an error
            if writeln!(stdin, "{i}\t{l}").is_err() {
                break;
            }
        }
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Ok(s.split('\t').next().and_then(|n| n.trim().parse().ok()))
}

fn numbered(lines: &[String]) -> Result<Option<usize>> {
    for (i, l) in lines.iter().enumerate() {
        outln!("{:>3}  {l}", i + 1);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(None);
    }
    out!("play # (enter to quit)> ");
    crate::flush()?;
    let mut input = String::new();
    std::io::stdin().lock().read_line(&mut input)?;
    Ok(input
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|n| (1..=lines.len()).contains(n))
        .map(|n| n - 1))
}
