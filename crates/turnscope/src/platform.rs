//! Platform-specific paths, permissions, file identities, and shell helpers.
//! Core engine modules call into this module instead of importing `std::os::*`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
const HOME_VAR: &str = "USERPROFILE";
#[cfg(not(windows))]
const HOME_VAR: &str = "HOME";

pub(crate) fn home_var() -> &'static str {
    HOME_VAR
}

pub(crate) fn home() -> PathBuf {
    std::env::var_os(HOME_VAR)
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub(crate) fn is_own_home(home: &Path) -> bool {
    std::env::var_os(HOME_VAR).is_some_and(|own| Path::new(&own) == home)
}

pub(crate) fn data_dir(home: &Path) -> PathBuf {
    home.join("Library/Application Support/com.joeychilson.turnscope")
}

pub(crate) fn set_home(command: &mut Command, home: &Path) {
    command.env(HOME_VAR, home);
}

/// Create `dir` (and any missing parents) accessible only to the owner (0700 on Unix).
pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// A stable per-file identifier on the current volume (Unix inode; 0 elsewhere).
pub(crate) fn file_id(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

/// Whether `program` resolves to an executable file on the current `PATH`.
pub(crate) fn on_path(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| is_executable(&dir.join(program)))
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// An app started from Finder runs with a bare `PATH`, so the agent's
/// command is found with the `PATH` of the person's interactive login shell,
/// in their own home: on 2026-09-30, `zsh -lc` found none of four agents,
/// and `zsh -ilc` all four, in 0.15 s.
#[cfg(unix)]
pub(crate) fn login_path() -> Option<String> {
    let env_shell = std::env::var("SHELL").ok();
    let (shell, script) = login_shell(env_shell.as_deref());
    let mut command = Command::new(shell);
    command.args(["-ilc", script]);
    let output =
        crate::process::output_within(&mut command, std::time::Duration::from_secs(10)).ok()?;
    if !output.status.success() {
        return None;
    }
    extract_path(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(unix))]
pub(crate) fn login_path() -> Option<String> {
    None
}

#[cfg(any(unix, test))]
fn login_shell(shell: Option<&str>) -> (&str, &'static str) {
    const POSIX: &str = "printf '__path__=%s__end__' \"$PATH\"";
    const FISH: &str = "printf '__path__=%s__end__' (string join : $PATH)";
    let shell = shell.filter(|s| !s.is_empty()).unwrap_or("/bin/zsh");
    match Path::new(shell).file_name().and_then(|name| name.to_str()) {
        Some("fish") => (shell, FISH),
        // Known non-POSIX interactive shells where `-ilc` + POSIX `printf` fail.
        Some("tcsh" | "csh" | "nu" | "nushell" | "elvish" | "pwsh" | "xonsh" | "ion") => {
            ("/bin/zsh", POSIX)
        }
        _ => (shell, POSIX),
    }
}

#[cfg(any(unix, test))]
fn extract_path(said: &str) -> Option<String> {
    let (_, after) = said.rsplit_once("__path__=")?;
    let (path, _) = after.split_once("__end__")?;
    let path = path.trim();
    (!path.is_empty()).then(|| path.to_owned())
}

/// Why `exe` is in a temporary location an agent shouldn't be pointed at, if
/// it is: on macOS, a disk image or removable drive under `/Volumes`, or App
/// Translocation's read-only copy.
#[cfg(target_os = "macos")]
pub(crate) fn temporary_install(exe: &Path) -> Option<String> {
    (exe.starts_with("/Volumes") || exe.to_string_lossy().contains("/AppTranslocation/")).then(
        || {
            format!(
                "Turnscope's engine runs from {}, which goes away when its disk image is ejected or its drive is removed. Put Turnscope in Applications, quit it, and open it from there to connect",
                exe.display()
            )
        },
    )
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn temporary_install(_exe: &Path) -> Option<String> {
    None
}

/// Format `cmd` with `env` variables and `args` as a command a person can
/// paste into their shell, escaping single quotes safely.
pub(crate) fn shell_command(env: &[(&str, &str)], cmd: &str, args: &[&str]) -> String {
    let mut parts = Vec::with_capacity(env.len() + 1 + args.len());
    for (var, value) in env {
        parts.push(format!("{var}={}", quote(value)));
    }
    parts.push(cmd.to_owned());
    for arg in args {
        parts.push(if plain_arg(arg) {
            (*arg).to_owned()
        } else {
            quote(arg)
        });
    }
    parts.join(" ")
}

fn plain_arg(arg: &str) -> bool {
    !arg.is_empty()
        && arg
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_path_ignores_leading_and_trailing_shell_output() {
        assert_eq!(
            extract_path("welcome\n__path__=/opt/homebrew/bin:/usr/bin__end__\ngoodbye\n"),
            Some("/opt/homebrew/bin:/usr/bin".to_owned())
        );
        assert_eq!(extract_path("__path__=__end__"), None);
        assert_eq!(extract_path("__path__=/usr/bin"), None);
    }

    #[test]
    fn login_shell_handles_fish_and_non_posix_shells() {
        let (shell, script) = login_shell(Some("/opt/homebrew/bin/fish"));
        assert_eq!(shell, "/opt/homebrew/bin/fish");
        assert!(script.contains("string join : $PATH"), "{script}");
        for non_posix in ["/opt/homebrew/bin/nu", "/bin/tcsh", "/usr/local/bin/elvish"] {
            let (shell, script) = login_shell(Some(non_posix));
            assert_eq!(shell, "/bin/zsh", "{non_posix}");
            assert!(script.contains("\"$PATH\""), "{script}");
        }
        assert_eq!(login_shell(Some("/usr/bin/false")).0, "/usr/bin/false");
    }

    #[test]
    fn shell_command_escapes_single_quotes_in_env_and_args() {
        let cmd = shell_command(
            &[
                ("HOME", "/Users/O'Connor"),
                ("CODEX_HOME", "/Users/O'Connor/.codex-work"),
            ],
            "codex",
            &[
                "mcp",
                "add",
                "turnscope",
                "--",
                "/Users/O'Connor/turnscope",
                "mcp",
            ],
        );
        assert_eq!(
            cmd,
            r"HOME='/Users/O'\''Connor' CODEX_HOME='/Users/O'\''Connor/.codex-work' codex mcp add turnscope -- '/Users/O'\''Connor/turnscope' mcp"
        );
    }

    #[cfg(unix)]
    #[test]
    fn on_path_distinguishes_executables_from_plain_files() {
        assert!(on_path("sh"));
        assert!(!on_path("turnscope-nonexistent-binary-for-test"));
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain");
        std::fs::write(&file, "not executable").unwrap();
        assert!(!is_executable(&file));
    }

    #[cfg(unix)]
    #[test]
    fn create_private_dir_sets_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("private/nested");
        create_private_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn temporary_install_flags_disk_images_and_translocated_apps() {
        for (exe, temporary) in [
            (
                "/Volumes/Turnscope/Turnscope.app/Contents/Helpers/turnscope",
                true,
            ),
            (
                "/private/var/folders/ab/T/AppTranslocation/0A1B/d/Turnscope.app/Contents/Helpers/turnscope",
                true,
            ),
            (
                "/Applications/Turnscope.app/Contents/Helpers/turnscope",
                false,
            ),
            ("/Users/me/Volumes/target/release/turnscope", false),
        ] {
            assert_eq!(
                temporary_install(Path::new(exe)).is_some(),
                temporary,
                "{exe}"
            );
        }
    }
}
