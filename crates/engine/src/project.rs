//! Which project a working directory belongs to.
//!
//! A session belongs to the repository its working directory is in, not to the
//! folder: a subdirectory, and a git worktree checked out elsewhere, belong to
//! the repository's main working tree. A submodule is a project of its own. A
//! directory in no repository, or no longer there, is its own project.
//!
//! A worktree an agent keeps inside the repository, as Claude Code keeps its
//! subagents' in `.claude/worktrees/`, belongs to that repository by its path
//! alone: the agent removes it when the subagent is done, often before its
//! session is read, and git's record of it with it. Resolved through git
//! alone, 48 such worktrees on this Mac had each become a project of its
//! own (2026-09-23).
//!
//! The search stops below the home directory, so a home kept in git does not
//! make everything beneath it one project.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// Where agents keep worktrees inside the repository they belong to.
const NESTED_WORKTREES: &str = "/.claude/worktrees/";

/// The folder holding an agent's worktree `path` is in, when it is in one:
/// `/work/ledger` for `/work/ledger/.claude/worktrees/agent-a1/src`.
fn host(path: &str) -> Option<&str> {
    path.find(NESTED_WORKTREES)
        .map(|at| &path[..at])
        .filter(|host| !host.is_empty())
}

/// The project `path` belongs to, as its root directory and its name.
pub(crate) fn resolve(path: &str, home: &Path) -> (String, String) {
    if let Some(host) = host(path) {
        return resolve(host, home);
    }
    let start = Path::new(path);
    for directory in start.ancestors() {
        if directory == home || directory.parent().is_none() {
            break;
        }
        let git = directory.join(".git");
        let Ok(metadata) = fs::symlink_metadata(&git) else {
            continue;
        };
        let root = if metadata.is_dir() {
            directory.to_path_buf()
        } else if metadata.is_file() {
            main_tree(directory, &git).unwrap_or_else(|| directory.to_path_buf())
        } else {
            continue;
        };
        return spelled(&root.to_string_lossy());
    }
    spelled(path)
}

/// The main working tree of the worktree at `directory`, whose `.git` file
/// points into the main repository's `.git/worktrees/`. `None` for anything
/// else a `.git` file can point to, such as a submodule's directory.
fn main_tree(directory: &Path, git: &Path) -> Option<PathBuf> {
    let pointer = fs::read_to_string(git).ok()?;
    let target = pointer.trim().strip_prefix("gitdir:")?.trim();
    let target = directory.join(target);
    // A worktree's directory holds `commondir`, the path to the repository's
    // own `.git` from there.
    let common = fs::read_to_string(target.join("commondir")).ok()?;
    let common = normalize(&target.join(common.trim()));
    if common.file_name()? != ".git" {
        return None;
    }
    common.parent().map(Path::to_path_buf)
}

/// `path` with `.` and `..` resolved without touching the file system.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

/// The project at `root`, as its folder is spelled on disk, and its name.
/// APFS finds `/work/Ledger` at `/work/ledger`, and a shell keeps whichever
/// spelling was typed, so an agent started from each would record a project
/// of its own; none has on this Mac (48 projects, 2026-09-27). Only a
/// spelling that differs by the case of letters alone is taken: a root
/// reached through a link keeps its own, and one no longer there the one it
/// was found by.
pub(crate) fn spelled(root: &str) -> (String, String) {
    let on_disk = fs::canonicalize(root)
        .ok()
        .filter(|found| found.to_string_lossy().to_lowercase() == root.to_lowercase());
    named(on_disk.as_deref().unwrap_or(Path::new(root)))
}

/// A project's root as text, and its name: the root's last component.
fn named(root: &Path) -> (String, String) {
    let text = root.to_string_lossy().into_owned();
    let name = root
        .file_name()
        .map_or_else(|| text.clone(), |name| name.to_string_lossy().into_owned());
    (text, name)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{resolve, spelled};

    #[test]
    fn a_folder_reached_by_another_case_is_one_project_spelled_as_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        // The temporary folder is itself reached through a link, /var to
        // /private/var, which the check of spelling would keep.
        let base = fs::canonicalize(dir.path()).unwrap();
        let repository = base.join("Work/Ledger");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(repository.join("src")).unwrap();
        let typed = base.join("work/ledger/src");
        assert_eq!(
            resolve(&typed.to_string_lossy(), &base),
            (
                repository.to_string_lossy().into_owned(),
                "Ledger".to_owned()
            )
        );
    }

    #[test]
    fn a_root_reached_through_a_link_keeps_its_own_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(base.join("Volumes/Code/ledger")).unwrap();
        std::os::unix::fs::symlink(base.join("Volumes/Code"), base.join("code")).unwrap();
        let linked = base.join("code/ledger");
        assert_eq!(
            spelled(&linked.to_string_lossy()),
            (linked.to_string_lossy().into_owned(), "ledger".to_owned())
        );
    }

    #[test]
    fn a_subdirectory_belongs_to_its_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repository = dir.path().join("work/ledger");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(repository.join("src/api")).unwrap();
        let (root, name) = resolve(&repository.join("src/api").to_string_lossy(), dir.path());
        assert_eq!(
            (root, name.as_str()),
            (repository.to_string_lossy().into_owned(), "ledger")
        );
    }

    #[test]
    fn a_worktree_belongs_to_its_main_repository_and_a_submodule_to_itself() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("work/ledger");
        let entry = main.join(".git/worktrees/idempotency");
        fs::create_dir_all(&entry).unwrap();
        fs::write(entry.join("commondir"), "../..\n").unwrap();
        let worktree = dir.path().join("work/ledger-idempotency");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", entry.display()),
        )
        .unwrap();
        assert_eq!(resolve(&worktree.to_string_lossy(), dir.path()).1, "ledger");

        let module = main.join("vendor/pricing");
        fs::create_dir_all(main.join(".git/modules/pricing")).unwrap();
        fs::create_dir_all(&module).unwrap();
        fs::write(module.join(".git"), "gitdir: ../../.git/modules/pricing\n").unwrap();
        assert_eq!(resolve(&module.to_string_lossy(), dir.path()).1, "pricing");
    }

    #[test]
    fn an_agents_worktree_belongs_to_the_repository_holding_it_even_once_removed() {
        let dir = tempfile::tempdir().unwrap();
        let repository = dir.path().join("work/ledger");
        fs::create_dir_all(repository.join(".git")).unwrap();
        let worktree = repository.join(".claude/worktrees/agent-a1b2/src");
        let (root, name) = resolve(&worktree.to_string_lossy(), dir.path());
        assert_eq!(
            (root, name.as_str()),
            (repository.to_string_lossy().into_owned(), "ledger")
        );
        // With the repository gone too, the folder that held it stands for it.
        assert_eq!(
            resolve("/no/longer/ledger/.claude/worktrees/agent-a1b2", dir.path()),
            ("/no/longer/ledger".to_owned(), "ledger".to_owned())
        );
    }

    #[test]
    fn a_directory_in_no_repository_is_its_own_project_even_under_a_home_in_git() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let loose = dir.path().join("scratch/notes");
        fs::create_dir_all(&loose).unwrap();
        assert_eq!(resolve(&loose.to_string_lossy(), dir.path()).1, "notes");
        assert_eq!(resolve("/no/longer/here", dir.path()).1, "here");
    }
}
