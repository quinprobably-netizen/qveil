//! A yt-dlp that is already started, for the next call to use.
//!
//! yt-dlp is a Python program, and before it does anything it spends a
//! quarter of a second (a second, when the machine is busy) starting Python
//! and importing itself. That is the same work every time, so one process is
//! kept that has done it and waits on stdin for what to do; a call hands it
//! its arguments there, and it runs yt-dlp's own entry script on them, in
//! the same process. Everything else is as for a call started the usual way
//! (see `Inflight::claim_spare`): it is started through the route's registry,
//! in a process group of its own and killed with the engine, its arguments
//! are the same and its private files too.
//!
//! This is only done when yt-dlp is the usual entry script, or a zipapp, run
//! by a Python named in its first line. Anything else is started as it is.

use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// What the spare writes to stderr once it is ready.
pub const READY: &[u8] = b"qveil-spare-ready\n";

/// How the spare is started: `python -P -c BOOT <script> <zipped>`.
///
/// `-P` keeps the current directory off `sys.path`; the script's own
/// directory (the zipapp itself) is put first instead, which is what Python
/// does when it runs the script. Once it has its arguments, it checks that
/// yt-dlp wasn't replaced (an upgrade) since it was imported, and if it was,
/// runs the script afresh in this same process (`execv`) instead.
const BOOT: &str = r#"import os, sys
script, zipped = sys.argv[1], sys.argv[2] == "1"
sys.path.insert(0, script if zipped else os.path.dirname(os.path.realpath(script)))
import yt_dlp
try:
    import yt_dlp.extractor.youtube
except Exception:
    pass
g = getattr(sys.modules.get("yt_dlp.globals"), "all_plugins_loaded", None)
if g is not None and getattr(g, "value", False):
    sys.exit(3)
def stamp():
    return [(s.st_ino, s.st_size, s.st_mtime_ns) for s in map(os.stat, (script, yt_dlp.__file__))]
before = stamp()
os.write(2, b"qveil-spare-ready\n")
line = sys.stdin.buffer.readline()
if not line:
    sys.exit(0)
import json
args = json.loads(line)
null = os.open(os.devnull, os.O_RDONLY)
os.dup2(null, 0)
os.close(null)
if stamp() != before:
    os.execv(sys.executable, [sys.executable, script] + args)
sys.argv = [script] + args
import runpy
runpy.run_path(script, run_name="__main__")
"#;

/// A Python and the yt-dlp script it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    python: PathBuf,
    script: PathBuf,
    zipped: bool,
}

impl Launch {
    /// The spare's command, with stdin piped (where its arguments come).
    pub fn command(&self) -> Command {
        let mut c = Command::new(&self.python);
        c.arg("-P")
            .arg("-c")
            .arg(BOOT)
            .arg(&self.script)
            .arg(if self.zipped { "1" } else { "0" })
            .stdin(Stdio::piped());
        c
    }
}

#[cfg(test)]
impl Launch {
    /// A launch that is only ever compared with another, never run.
    pub fn testing(name: &str) -> Launch {
        Launch {
            python: PathBuf::from("/nowhere/python3"),
            script: PathBuf::from(name),
            zipped: false,
        }
    }
}

/// How `bin` (a name looked up in `PATH`, or a path) could be run as a
/// spare, if it can.
pub fn launch_for(bin: &str) -> Option<Launch> {
    let script = find(bin)?;
    let mut text = Vec::new();
    std::fs::File::open(&script)
        .ok()?
        .take(8 << 10)
        .read_to_end(&mut text)
        .ok()?;
    let newline = text.iter().position(|&b| b == b'\n')?;
    let first = std::str::from_utf8(text[..newline].strip_prefix(b"#!")?).ok()?;
    let words: Vec<&str> = first.split_whitespace().collect();
    let python = match words.as_slice() {
        [interp] if is_python(interp) => PathBuf::from(interp),
        [env, name] if Path::new(env).file_name() == Some(OsStr::new("env")) && is_python(name) => {
            find(name)?
        }
        // Options for the interpreter would have to be passed on too.
        _ => return None,
    };
    let rest = &text[newline + 1..];
    let zipped = rest.starts_with(b"PK\x03\x04");
    // The entry script that pip and the distributions install. It is short:
    // anything longer might do more before it imports yt-dlp.
    let entry = text.len() < 2048
        && std::str::from_utf8(rest).is_ok_and(|t| t.contains("from yt_dlp import main"));
    (python.is_file() && (zipped || entry)).then_some(Launch {
        python,
        script,
        zipped,
    })
}

/// `python`, `python3` or `python3.12`, as a path or a name.
fn is_python(interp: &str) -> bool {
    let name = Path::new(interp)
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("");
    let Some(version) = name.strip_prefix("python") else {
        return false;
    };
    version.is_empty()
        || version
            .split('.')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Where `bin` is: itself if it names a path, or else the first executable
/// file of that name in `PATH`, the way `Command` finds it.
fn find(bin: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if bin.contains('/') {
        let p = PathBuf::from(bin);
        return runnable(&p).then_some(p);
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(bin))
        .find(|p| runnable(p))
}

/// What `cmd` asks for, as the line the spare reads: its arguments as a JSON
/// list. `None` if the spare couldn't do the same: an argument that isn't
/// UTF-8, or an environment or a directory of its own.
pub fn args_line(cmd: &Command) -> Option<String> {
    if cmd.get_envs().next().is_some() || cmd.get_current_dir().is_some() {
        return None;
    }
    let args: Option<Vec<&str>> = cmd.get_args().map(OsStr::to_str).collect();
    let mut line = serde_json::to_string(&args?).ok()?;
    line.push('\n');
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::testing::TempDir;
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, text: &[u8]) -> String {
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn only_the_usual_entry_script_or_a_zipapp_under_a_python_is_kept_spare() {
        let dir = TempDir::new();
        let python = write(&dir.0.join("python3.12"), b"#!/bin/sh\n");
        let entry = "import re\nimport sys\nfrom yt_dlp import main\nif __name__ == '__main__':\n    sys.exit(main())\n";
        let script = write(
            &dir.0.join("yt-dlp"),
            format!("#!{python}\n{entry}").as_bytes(),
        );
        assert_eq!(
            launch_for(&script),
            Some(Launch {
                python: python.clone().into(),
                script: script.clone().into(),
                zipped: false
            })
        );
        let zipapp = write(
            &dir.0.join("zipped"),
            format!("#!{python}\nPK\x03\x04rest").as_bytes(),
        );
        assert!(launch_for(&zipapp).unwrap().zipped);
        for (name, text) in [
            ("shell", format!("#!/bin/sh\n{entry}")),
            ("options", format!("#!{python} -I\n{entry}")),
            (
                "not-python",
                format!("#!{}\n{entry}", dir.0.join("ruby").display()),
            ),
            ("missing", format!("#!/nowhere/python3\n{entry}")),
            ("other", format!("#!{python}\nimport os\nos.system('x')\n")),
            ("long", format!("#!{python}\n{entry}{}", "#\n".repeat(2000))),
        ] {
            let bin = write(&dir.0.join(name), text.as_bytes());
            assert_eq!(launch_for(&bin), None, "{name}");
        }
        assert_eq!(
            launch_for(&dir.0.join("absent").display().to_string()),
            None
        );
        assert!(is_python("/usr/bin/python") && is_python("python3") && is_python("python3.14"));
        assert!(!is_python("python3-config") && !is_python("pythonx") && !is_python("ipython"));
    }

    #[test]
    fn the_arguments_go_as_one_json_line_and_only_when_nothing_else_differs() {
        let mut c = Command::new("x");
        c.args(["--proxy-free", "it's \"quoted\"\n"]);
        assert_eq!(
            args_line(&c).unwrap(),
            "[\"--proxy-free\",\"it's \\\"quoted\\\"\\n\"]\n"
        );
        assert_eq!(args_line(Command::new("x").env("A", "b")), None);
        assert_eq!(args_line(Command::new("x").current_dir("/")), None);
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            args_line(Command::new("x").arg(OsStr::from_bytes(b"\xff"))),
            None
        );
    }
}
