use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    Link(LinkEntry),
    Config(FileEntry),
    Script(FileEntry),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkEntry {
    pub source: PathBuf,
    pub target: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub source: PathBuf,
    pub target: PathBuf,
    pub mode: Option<u32>,
    #[serde(default)]
    pub sudo: bool,
}

impl Entry {
    pub fn source(&self) -> &Path {
        match self {
            Self::Link(entry) => &entry.source,
            Self::Config(entry) | Self::Script(entry) => &entry.source,
        }
    }

    pub fn target(&self) -> &Path {
        match self {
            Self::Link(entry) => &entry.target,
            Self::Config(entry) | Self::Script(entry) => &entry.target,
        }
    }

    fn effective_mode(&self) -> Result<Option<u32>, Error> {
        match self {
            Self::Link(_) => Ok(None),
            Self::Config(entry) => {
                let mode = entry.mode.unwrap_or(0o644);
                validate_file_mode(mode, FileKind::Config)?;
                Ok(Some(mode))
            }
            Self::Script(entry) => {
                let mode = entry.mode.unwrap_or(0o755);
                validate_file_mode(mode, FileKind::Script)?;
                Ok(Some(mode))
            }
        }
    }

    fn uses_sudo(&self) -> bool {
        match self {
            Self::Link(_) => false,
            Self::Config(entry) | Self::Script(entry) => entry.sudo,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryStatus {
    Ok,
    Missing,
    Modified,
    Conflict,
    Unreadable,
}

impl fmt::Display for EntryStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok => f.write_str("ok"),
            Self::Missing => f.write_str("missing"),
            Self::Modified => f.write_str("modified"),
            Self::Conflict => f.write_str("conflict"),
            Self::Unreadable => f.write_str("unreadable"),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    #[serde(default, rename = "link")]
    links: Vec<LinkEntry>,
    #[serde(default, rename = "config")]
    configs: Vec<FileEntry>,
    #[serde(default, rename = "script")]
    scripts: Vec<FileEntry>,
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Config(toml::de::Error),
    Invalid(String),
    Conflict(PathBuf),
    CommandFailed {
        program: &'static str,
        code: Option<i32>,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Config(error) => write!(f, "invalid configuration: {error}"),
            Self::Invalid(message) => f.write_str(message),
            Self::Conflict(path) => write!(
                f,
                "refusing to replace conflicting path {} (use --force)",
                path.display()
            ),
            Self::CommandFailed { program, code } => write!(
                f,
                "{program} failed with exit status {}",
                code.map_or_else(|| "unknown".into(), |code| code.to_string())
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<toml::de::Error> for Error {
    fn from(value: toml::de::Error) -> Self {
        Self::Config(value)
    }
}

pub fn load_entries(config_path: &Path) -> Result<Vec<Entry>, Error> {
    let text = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&text)?;
    let repository = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let home = env::var_os("HOME").map(PathBuf::from);
    let mut entries =
        Vec::with_capacity(config.links.len() + config.configs.len() + config.scripts.len());

    for mut entry in config.links {
        entry.source = resolve_source(&repository, &entry.source, false)?;
        entry.target = expand_target(&entry.target, home.as_deref())?;
        entries.push(Entry::Link(entry));
    }
    for mut entry in config.configs {
        entry.source = resolve_source(&repository, &entry.source, true)?;
        entry.target = expand_target(&entry.target, home.as_deref())?;
        entries.push(Entry::Config(entry));
    }
    for mut entry in config.scripts {
        entry.source = resolve_source(&repository, &entry.source, true)?;
        entry.target = expand_target(&entry.target, home.as_deref())?;
        entries.push(Entry::Script(entry));
    }

    let mut targets = HashSet::new();
    for entry in &entries {
        validate_entry(entry)?;
        if !targets.insert(entry.target().to_path_buf()) {
            return Err(Error::Invalid(format!(
                "duplicate target in configuration: {}",
                entry.target().display()
            )));
        }
    }
    Ok(entries)
}

pub fn find_config_upwards(start: &Path) -> Result<PathBuf, Error> {
    let mut directory = start.canonicalize().map_err(|error| {
        Error::Invalid(format!(
            "invalid configuration search path {}: {error}",
            start.display()
        ))
    })?;
    loop {
        let candidate = directory.join("dotfiles.toml");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !directory.pop() {
            return Err(Error::Invalid(format!(
                "could not find dotfiles.toml from {}",
                start.display()
            )));
        }
    }
}

fn resolve_source(repository: &Path, source: &Path, must_be_file: bool) -> Result<PathBuf, Error> {
    if source.as_os_str().is_empty() {
        return Err(Error::Invalid("source path must not be empty".into()));
    }
    if source
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(Error::Invalid(format!(
            "source path must not contain '..': {}",
            source.display()
        )));
    }

    let candidate = if source.is_absolute() {
        source.to_path_buf()
    } else {
        repository.join(source)
    };
    let resolved = candidate.canonicalize().map_err(|error| {
        Error::Invalid(format!("invalid source {}: {error}", candidate.display()))
    })?;
    if !resolved.starts_with(repository) {
        return Err(Error::Invalid(format!(
            "source is outside the dotfiles repository: {}",
            source.display()
        )));
    }
    if must_be_file && !resolved.is_file() {
        return Err(Error::Invalid(format!(
            "source is not a regular file: {}",
            resolved.display()
        )));
    }
    Ok(resolved)
}

fn expand_target(target: &Path, home: Option<&Path>) -> Result<PathBuf, Error> {
    let raw = target.to_str().ok_or_else(|| {
        Error::Invalid(format!("target is not valid UTF-8: {}", target.display()))
    })?;
    let expanded = if raw == "~" || raw == "$HOME" {
        home.ok_or_else(|| Error::Invalid("HOME is not set".into()))?
            .to_path_buf()
    } else if let Some(suffix) = raw.strip_prefix("~/") {
        home.ok_or_else(|| Error::Invalid("HOME is not set".into()))?
            .join(suffix)
    } else if let Some(suffix) = raw.strip_prefix("$HOME/") {
        home.ok_or_else(|| Error::Invalid("HOME is not set".into()))?
            .join(suffix)
    } else {
        target.to_path_buf()
    };
    Ok(expanded)
}

fn validate_entry(entry: &Entry) -> Result<(), Error> {
    validate_target(entry.target())?;
    entry.effective_mode()?;
    match entry {
        Entry::Link(_) if !entry.source().exists() => Err(Error::Invalid(format!(
            "link source does not exist: {}",
            entry.source().display()
        ))),
        Entry::Config(_) | Entry::Script(_) if !entry.source().is_file() => Err(Error::Invalid(
            format!("source is not a regular file: {}", entry.source().display()),
        )),
        _ => Ok(()),
    }
}

fn validate_target(target: &Path) -> Result<(), Error> {
    if target.as_os_str().is_empty() || !target.is_absolute() {
        return Err(Error::Invalid(format!(
            "target must be an absolute path (or start with ~/$HOME): {}",
            target.display()
        )));
    }
    if target == Path::new("/") || target == Path::new("/etc") || target == Path::new("/usr") {
        return Err(Error::Invalid(format!(
            "refusing dangerous target path: {}",
            target.display()
        )));
    }
    if target
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(Error::Invalid(format!(
            "target path must not contain '..': {}",
            target.display()
        )));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum FileKind {
    Config,
    Script,
}

fn validate_file_mode(mode: u32, kind: FileKind) -> Result<(), Error> {
    if mode & !0o777 != 0 {
        return Err(Error::Invalid(format!(
            "mode {mode:#o} contains forbidden special or invalid bits"
        )));
    }
    match kind {
        FileKind::Config if mode & 0o111 != 0 => Err(Error::Invalid(format!(
            "config mode must not contain executable bits: {mode:#o}"
        ))),
        FileKind::Script if mode & 0o100 == 0 => Err(Error::Invalid(format!(
            "script mode must contain the owner executable bit: {mode:#o}"
        ))),
        _ => Ok(()),
    }
}

pub fn entry_status(entry: &Entry) -> Result<EntryStatus, Error> {
    validate_entry(entry)?;
    let metadata = match fs::symlink_metadata(entry.target()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(EntryStatus::Missing),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Ok(EntryStatus::Unreadable);
        }
        Err(error) => return Err(error.into()),
    };

    match entry {
        Entry::Link(_) => {
            if !metadata.file_type().is_symlink() {
                return Ok(EntryStatus::Conflict);
            }
            let actual = fs::read_link(entry.target())?;
            let actual = if actual.is_absolute() {
                actual
            } else {
                entry
                    .target()
                    .parent()
                    .unwrap_or_else(|| Path::new("/"))
                    .join(actual)
            };
            Ok(if paths_refer_to_same_file(&actual, entry.source()) {
                EntryStatus::Ok
            } else {
                EntryStatus::Conflict
            })
        }
        Entry::Config(_) | Entry::Script(_) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                return Ok(EntryStatus::Conflict);
            }
            let expected_mode = entry.effective_mode()?.expect("file entries have a mode");
            let mode_matches = metadata.permissions().mode() & 0o7777 == expected_mode;
            let owner_matches = !entry.uses_sudo() || (metadata.uid() == 0 && metadata.gid() == 0);
            if !mode_matches || !owner_matches {
                return Ok(EntryStatus::Modified);
            }
            let content_matches = match files_equal(entry.source(), entry.target()) {
                Ok(matches) => matches,
                Err(Error::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied => {
                    return Ok(EntryStatus::Unreadable);
                }
                Err(error) => return Err(error),
            };
            Ok(if content_matches {
                EntryStatus::Ok
            } else {
                EntryStatus::Modified
            })
        }
    }
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn files_equal(left: &Path, right: &Path) -> Result<bool, Error> {
    let left_meta = fs::metadata(left)?;
    let right_meta = fs::metadata(right)?;
    if left_meta.len() != right_meta.len() {
        return Ok(false);
    }
    Ok(fs::read(left)? == fs::read(right)?)
}

#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub force: bool,
}

pub fn install_entry(entry: &Entry, options: InstallOptions) -> Result<Vec<String>, Error> {
    validate_entry(entry)?;
    let status = entry_status(entry)?;
    if status == EntryStatus::Ok {
        return Ok(Vec::new());
    }
    if status == EntryStatus::Conflict && !options.force {
        return Err(Error::Conflict(entry.target().to_path_buf()));
    }

    let parent = entry.target().parent().ok_or_else(|| {
        Error::Invalid(format!(
            "target has no parent: {}",
            entry.target().display()
        ))
    })?;
    let sudo = entry.uses_sudo();
    let mut operations = Vec::new();

    if !parent.exists() {
        operations.push(format!("create directory {}", parent.display()));
        if !options.dry_run {
            if sudo {
                sudo_create_directory(parent)?;
            } else {
                fs::create_dir_all(parent)?;
            }
        }
    }

    if status == EntryStatus::Conflict {
        ensure_removable_conflict(entry.target())?;
        operations.push(format!("remove conflicting {}", entry.target().display()));
        if !options.dry_run {
            remove_target(entry.target(), sudo)?;
        }
    }

    match entry {
        Entry::Link(_) => {
            operations.push(format!(
                "link {} -> {}",
                entry.target().display(),
                entry.source().display()
            ));
            if !options.dry_run {
                symlink(entry.source(), entry.target())?;
            }
        }
        Entry::Config(_) | Entry::Script(_) => {
            let mode = entry.effective_mode()?.expect("file entries have a mode");
            operations.push(format!(
                "install {} -> {} (mode {mode:#o})",
                entry.source().display(),
                entry.target().display()
            ));
            if !options.dry_run {
                if sudo {
                    sudo_install_file(entry.source(), entry.target(), mode)?;
                } else {
                    fs::copy(entry.source(), entry.target())?;
                    fs::set_permissions(entry.target(), fs::Permissions::from_mode(mode))?;
                }
            }
        }
    }

    Ok(operations)
}

fn ensure_removable_conflict(path: &Path) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_dir()
        && !metadata.file_type().is_symlink()
        && fs::read_dir(path)?.next().transpose()?.is_some()
    {
        return Err(Error::Invalid(format!(
            "refusing to remove non-empty directory: {}",
            path.display()
        )));
    }
    Ok(())
}

fn remove_target(path: &Path, sudo: bool) -> Result<(), Error> {
    let metadata = fs::symlink_metadata(path)?;
    let directory = metadata.file_type().is_dir() && !metadata.file_type().is_symlink();
    if sudo {
        let program = if directory {
            "/usr/bin/rmdir"
        } else {
            "/usr/bin/rm"
        };
        run_sudo(program, &[OsStr::new("--"), path.as_os_str()])
    } else if directory {
        fs::remove_dir(path).map_err(Error::Io)
    } else {
        fs::remove_file(path).map_err(Error::Io)
    }
}

fn sudo_create_directory(path: &Path) -> Result<(), Error> {
    run_sudo(
        "/usr/bin/install",
        &[
            OsStr::new("-d"),
            OsStr::new("-m"),
            OsStr::new("0755"),
            OsStr::new("-o"),
            OsStr::new("root"),
            OsStr::new("-g"),
            OsStr::new("root"),
            OsStr::new("--"),
            path.as_os_str(),
        ],
    )
}

fn sudo_install_file(source: &Path, target: &Path, mode: u32) -> Result<(), Error> {
    let mode = format!("{mode:04o}");
    run_sudo(
        "/usr/bin/install",
        &[
            OsStr::new("-m"),
            OsStr::new(&mode),
            OsStr::new("-o"),
            OsStr::new("root"),
            OsStr::new("-g"),
            OsStr::new("root"),
            OsStr::new("--"),
            source.as_os_str(),
            target.as_os_str(),
        ],
    )
}

fn run_sudo(program: &'static str, args: &[&OsStr]) -> Result<(), Error> {
    let status = Command::new("/usr/bin/sudo")
        .arg(program)
        .args(args)
        .status()?;
    if !status.success() {
        return Err(Error::CommandFailed {
            program,
            code: status.code(),
        });
    }
    Ok(())
}

pub fn ensure_not_running_as_root() -> Result<(), Error> {
    let status = fs::read_to_string("/proc/self/status")?;
    let effective_uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        .and_then(|uid| uid.parse::<u32>().ok())
        .ok_or_else(|| Error::Invalid("could not determine effective user id".into()))?;
    if effective_uid == 0 {
        return Err(Error::Invalid(
            "refusing to run install as root; run magsys as a regular user".into(),
        ));
    }
    Ok(())
}
