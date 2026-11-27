//! The Claude Code skill carried inside the binary, and `sdm skill`.
//!
//! `cargo install` cannot run anything after it has placed the binary, so the
//! skill's files are embedded here and written out by `sdm skill install`.

use std::fmt;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use inquire::{InquireError, Select};
use serde_json::{Value, json};

use crate::error::{CliError, Result};
use crate::{credentials, output};

/// Folder name of the skill inside a `skills` directory.
const NAME: &str = "sdm-cli";
/// The skill: file name and content.
const FILES: [(&str, &str); 2] = [
    ("SKILL.md", include_str!("../skill/SKILL.md")),
    ("AGENTS.md", include_str!("../AGENTS.md")),
];
/// Set to silence the hint about a missing or outdated skill.
const NO_HINT_ENV: &str = "SDM_NO_SKILL_HINT";

/// Where a skill can be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// The user's Claude Code folder: every project of this user.
    User,
    /// `.claude` in the current directory: this project only.
    Project,
}

impl Location {
    fn dir(self) -> Result<PathBuf> {
        let base = match self {
            Self::User => claude_dir()
                .ok_or_else(|| CliError::usage("cannot locate a home directory for the skill"))?,
            Self::Project => current_dir()?.join(".claude"),
        };
        Ok(skill_dir(&base))
    }

    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

struct Choice {
    location: Location,
    dir: PathBuf,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location.label(), self.dir.display())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Absent,
    Outdated,
    Current,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Outdated => "outdated",
            Self::Current => "current",
        }
    }
}

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn claude_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
}

fn skill_dir(claude_dir: &Path) -> PathBuf {
    claude_dir.join("skills").join(NAME)
}

fn current_dir() -> Result<PathBuf> {
    std::env::current_dir()
        .map_err(|err| CliError::usage(format!("cannot read the current directory: {err}")))
}

/// Compares the files in `dir` with the ones in this binary.
fn state(dir: &Path) -> State {
    let on_disk: Vec<Option<String>> = FILES
        .iter()
        .map(|(name, _)| std::fs::read_to_string(dir.join(name)).ok())
        .collect();
    if on_disk.iter().all(Option::is_none) {
        State::Absent
    } else if on_disk
        .iter()
        .zip(FILES)
        .all(|(found, (_, text))| found.as_deref() == Some(text))
    {
        State::Current
    } else {
        State::Outdated
    }
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// Removes a symbolic link itself, never what it points to.
fn remove_link(path: &Path) -> Result<()> {
    std::fs::remove_file(path)
        .or_else(|_| std::fs::remove_dir(path))
        .map_err(|err| CliError::usage(format!("cannot remove {}: {err}", path.display())))
}

fn choose(location: Option<Location>, verb: &str) -> Result<Location> {
    if let Some(location) = location {
        return Ok(location);
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Err(CliError::usage(format!(
            "pass --user or --project to say where to {verb} the skill"
        )));
    }
    let choices = [Location::User, Location::Project]
        .into_iter()
        .map(|location| {
            Ok(Choice {
                location,
                dir: location.dir()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Select::new(&format!("Where to {verb} the skill?"), choices)
        .prompt()
        .map(|choice| choice.location)
        .map_err(|err| match err {
            InquireError::OperationCanceled | InquireError::OperationInterrupted => {
                CliError::usage("cancelled; nothing was changed")
            }
            other => CliError::usage(format!("cannot prompt: {other}")),
        })
}

/// `sdm skill install`: writes the skill's files into the chosen location.
///
/// # Errors
/// Returns a usage error when no location is given without a terminal, when
/// a symbolic link is in the way and `force` is not set, or when the files
/// cannot be written.
pub fn install(location: Option<Location>, force: bool) -> Result<()> {
    let dir = choose(location, "install")?.dir()?;
    let files: Vec<PathBuf> = FILES.iter().map(|(name, _)| dir.join(name)).collect();

    // Writing through a link would change a file somewhere else (a checkout
    // of this repository, for one), so links are replaced, and only on request.
    if let Some(link) = std::iter::once(&dir)
        .chain(&files)
        .find(|path| is_symlink(path))
        && !force
    {
        return Err(CliError::usage(format!(
            "{} is a symbolic link; pass --force to replace it with a copy",
            link.display()
        )));
    }
    if is_symlink(&dir) {
        remove_link(&dir)?;
    }
    let io_err = |path: &Path, err: std::io::Error| {
        CliError::usage(format!("cannot write {}: {err}", path.display()))
    };
    std::fs::create_dir_all(&dir).map_err(|err| io_err(&dir, err))?;
    for (file, (_, text)) in files.iter().zip(FILES) {
        if is_symlink(file) {
            remove_link(file)?;
        }
        std::fs::write(file, text).map_err(|err| io_err(file, err))?;
    }
    output::print_json(&json!({
        "installed": dir.display().to_string(),
        "files": FILES.map(|(name, _)| name),
        "version": env!("CARGO_PKG_VERSION"),
    }));
    Ok(())
}

/// `sdm skill status`: where the skill is installed and whether it matches
/// this binary.
///
/// # Errors
/// Returns a usage error when a location cannot be worked out.
pub fn status() -> Result<()> {
    let entry = |location: Location| -> Result<Value> {
        let dir = location.dir()?;
        Ok(json!({ "path": dir.display().to_string(), "state": state(&dir).as_str() }))
    };
    output::print_json(&json!({
        "version": env!("CARGO_PKG_VERSION"),
        "user": entry(Location::User)?,
        "project": entry(Location::Project)?,
    }));
    Ok(())
}

/// `sdm skill uninstall`: removes the skill's files, and its folder when
/// nothing else is in it.
///
/// # Errors
/// Returns a usage error when no location is given without a terminal or
/// when a file cannot be removed.
pub fn uninstall(location: Option<Location>) -> Result<()> {
    let dir = choose(location, "uninstall")?.dir()?;
    let mut removed = false;
    if is_symlink(&dir) {
        // The files belong to whatever the link points to: drop the link only.
        remove_link(&dir)?;
        removed = true;
    } else {
        for (name, _) in FILES {
            let file = dir.join(name);
            match std::fs::remove_file(&file) {
                Ok(()) => removed = true,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(CliError::usage(format!(
                        "cannot remove {}: {err}",
                        file.display()
                    )));
                }
            }
        }
        // Fails when the folder holds other files, which then stay.
        let _ = std::fs::remove_dir(&dir);
    }
    output::print_json(&json!({ "removed": removed, "path": dir.display().to_string() }));
    Ok(())
}

/// Tells a person at a terminal, once per version, that the skill is missing
/// or older than this binary. Says nothing to pipes, scripts and agents, or
/// on a machine without Claude Code.
pub fn hint() {
    if std::env::var_os(NO_HINT_ENV).is_some()
        || !(std::io::stdout().is_terminal() && std::io::stderr().is_terminal())
    {
        return;
    }
    let Some(claude) = claude_dir().filter(|dir| dir.is_dir()) else {
        return;
    };
    let Some(marker) = credentials::path()
        .ok()
        .and_then(|path| Some(path.parent()?.join("skill-hint")))
    else {
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    if std::fs::read_to_string(&marker).is_ok_and(|shown| shown.trim() == version) {
        return;
    }

    let mut states = vec![state(&skill_dir(&claude))];
    if let Ok(cwd) = std::env::current_dir() {
        states.extend(
            cwd.ancestors()
                .map(|dir| state(&skill_dir(&dir.join(".claude")))),
        );
    }
    if states.contains(&State::Current) {
        return;
    }
    let problem = if states.contains(&State::Outdated) {
        "is older than this sdm"
    } else {
        "is not installed"
    };
    eprintln!("hint: the Claude Code skill for sdm {problem}; run `sdm skill install`");
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, version);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_skill_has_frontmatter_and_names_its_guide() {
        let skill = FILES[0].1;
        assert!(skill.starts_with("---\nname: sdm-cli\n"));
        assert!(skill.contains("(AGENTS.md)"));
    }
}
