use magsys::{
    Entry, EntryStatus, FileEntry, InstallOptions, LinkEntry, entry_status, find_config_upwards,
    install_entry, load_entries,
};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("magsys-test-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source_file(root: &Path, name: &str) -> PathBuf {
    let source = root.join("repo").join(name);
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, "managed\n").unwrap();
    source
}

fn config_entry(root: &Path, mode: Option<u32>) -> Entry {
    Entry::Config(FileEntry {
        source: source_file(root, "config"),
        target: root.join("home/config"),
        mode,
        sudo: false,
    })
}

fn script_entry(root: &Path, mode: Option<u32>) -> Entry {
    Entry::Script(FileEntry {
        source: source_file(root, "script"),
        target: root.join("home/script"),
        mode,
        sudo: false,
    })
}

fn link_entry(root: &Path) -> Entry {
    let source = root.join("repo/niri");
    fs::create_dir_all(&source).unwrap();
    Entry::Link(LinkEntry {
        source,
        target: root.join("home/niri"),
    })
}

fn install(entry: &Entry, force: bool) -> Result<Vec<String>, magsys::Error> {
    install_entry(
        entry,
        InstallOptions {
            dry_run: false,
            force,
        },
    )
}

#[test]
fn parses_all_three_entry_types() {
    let temp = TempDir::new();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("niri")).unwrap();
    fs::create_dir_all(repo.join("zramen")).unwrap();
    fs::create_dir_all(repo.join("openrc")).unwrap();
    fs::write(repo.join("zramen/zramen.conf"), "config").unwrap();
    fs::write(repo.join("openrc/internal-keyboard-guard"), "script").unwrap();
    fs::write(
        repo.join("dotfiles.toml"),
        format!(
            "[[link]]\nsource = \"niri\"\ntarget = \"{}\"\n\n[[config]]\nsource = \"zramen/zramen.conf\"\ntarget = \"{}\"\nsudo = true\n\n[[script]]\nsource = \"openrc/internal-keyboard-guard\"\ntarget = \"{}\"\nsudo = true\n",
            temp.path().join("niri-target").display(),
            temp.path().join("config-target").display(),
            temp.path().join("script-target").display()
        ),
    )
    .unwrap();

    let entries = load_entries(&repo.join("dotfiles.toml")).unwrap();
    assert_eq!(entries.len(), 3);
    assert!(matches!(&entries[0], Entry::Link(_)));
    assert!(matches!(
        &entries[1],
        Entry::Config(FileEntry {
            mode: None,
            sudo: true,
            ..
        })
    ));
    assert!(matches!(
        &entries[2],
        Entry::Script(FileEntry {
            mode: None,
            sudo: true,
            ..
        })
    ));
}

#[test]
fn applies_default_modes() {
    let temp = TempDir::new();
    let config = config_entry(temp.path(), None);
    let script = script_entry(temp.path(), None);

    install(&config, false).unwrap();
    install(&script, false).unwrap();

    assert_eq!(
        fs::metadata(config.target()).unwrap().mode() & 0o7777,
        0o644
    );
    assert_eq!(
        fs::metadata(script.target()).unwrap().mode() & 0o7777,
        0o755
    );
    assert_eq!(entry_status(&config).unwrap(), EntryStatus::Ok);
    assert_eq!(entry_status(&script).unwrap(), EntryStatus::Ok);
}

#[test]
fn rejects_executable_config_mode() {
    let temp = TempDir::new();
    let entry = config_entry(temp.path(), Some(0o744));
    assert!(entry_status(&entry).is_err());
}

#[test]
fn rejects_non_executable_script_mode() {
    let temp = TempDir::new();
    let entry = script_entry(temp.path(), Some(0o644));
    assert!(entry_status(&entry).is_err());
}

#[test]
fn rejects_special_mode_bits() {
    for mode in [0o4644, 0o2644, 0o1644] {
        let temp = TempDir::new();
        let entry = config_entry(temp.path(), Some(mode));
        assert!(entry_status(&entry).is_err(), "mode {mode:#o} was accepted");
    }
    let temp = TempDir::new();
    let entry = script_entry(temp.path(), Some(0o4755));
    assert!(entry_status(&entry).is_err());
}

#[test]
fn rejects_mode_on_link() {
    let temp = TempDir::new();
    let repo = temp.path().join("repo");
    fs::create_dir_all(repo.join("niri")).unwrap();
    fs::write(
        repo.join("dotfiles.toml"),
        format!(
            "[[link]]\nsource = \"niri\"\ntarget = \"{}\"\nmode = 493\n",
            temp.path().join("target").display()
        ),
    )
    .unwrap();

    assert!(load_entries(&repo.join("dotfiles.toml")).is_err());
}

#[test]
fn installs_and_reports_a_directory_link() {
    let temp = TempDir::new();
    let entry = link_entry(temp.path());

    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Missing);
    install(&entry, false).unwrap();
    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Ok);
    assert_eq!(fs::read_link(entry.target()).unwrap(), entry.source());
}

#[test]
fn dry_run_does_not_change_the_filesystem() {
    let temp = TempDir::new();
    let entry = config_entry(temp.path(), Some(0o640));
    let operations = install_entry(
        &entry,
        InstallOptions {
            dry_run: true,
            force: false,
        },
    )
    .unwrap();

    assert_eq!(operations.len(), 2);
    assert!(!entry.target().exists());
    assert!(!entry.target().parent().unwrap().exists());
}

#[test]
fn conflict_requires_force() {
    let temp = TempDir::new();
    let entry = link_entry(temp.path());
    fs::create_dir_all(entry.target().parent().unwrap()).unwrap();
    fs::write(entry.target(), "unknown").unwrap();

    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Conflict);
    assert!(install(&entry, false).is_err());
    assert_eq!(fs::read_to_string(entry.target()).unwrap(), "unknown");
    install(&entry, true).unwrap();
    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Ok);
}

#[test]
fn force_does_not_remove_a_non_empty_directory() {
    let temp = TempDir::new();
    let entry = link_entry(temp.path());
    fs::create_dir_all(entry.target()).unwrap();
    fs::write(entry.target().join("unknown"), "data").unwrap();

    assert!(install(&entry, true).is_err());
    assert!(entry.target().join("unknown").exists());
}

#[test]
fn status_checks_file_content_and_mode() {
    let temp = TempDir::new();
    let entry = config_entry(temp.path(), Some(0o640));
    install(&entry, false).unwrap();

    fs::set_permissions(entry.target(), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Modified);
    fs::set_permissions(entry.target(), fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(entry.target(), "changed").unwrap();
    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Modified);
}

#[test]
fn rejects_sources_outside_repository() {
    let temp = TempDir::new();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let outside = temp.path().join("outside");
    fs::write(&outside, "data").unwrap();
    fs::write(
        repo.join("dotfiles.toml"),
        format!(
            "[[config]]\nsource = \"{}\"\ntarget = \"{}\"\n",
            outside.display(),
            temp.path().join("target").display()
        ),
    )
    .unwrap();

    assert!(load_entries(&repo.join("dotfiles.toml")).is_err());
}

#[test]
fn finds_config_in_parent_directory() {
    let temp = TempDir::new();
    let nested = temp.path().join("niri/deeper");
    fs::create_dir_all(&nested).unwrap();
    fs::write(temp.path().join("dotfiles.toml"), "").unwrap();

    assert_eq!(
        find_config_upwards(&nested).unwrap(),
        temp.path().join("dotfiles.toml")
    );
}

#[test]
fn parses_octal_modes() {
    let temp = TempDir::new();
    let repo = temp.path().join("repo");
    fs::create_dir(&repo).unwrap();
    fs::write(repo.join("config"), "data").unwrap();
    fs::write(repo.join("script"), "data").unwrap();
    fs::write(
        repo.join("dotfiles.toml"),
        format!(
            "[[config]]\nsource = \"config\"\ntarget = \"{}\"\nmode = 0o640\n\n[[script]]\nsource = \"script\"\ntarget = \"{}\"\nmode = 0o750\n",
            temp.path().join("config-target").display(),
            temp.path().join("script-target").display()
        ),
    )
    .unwrap();

    let entries = load_entries(&repo.join("dotfiles.toml")).unwrap();
    assert!(matches!(
        &entries[0],
        Entry::Config(FileEntry {
            mode: Some(0o640),
            ..
        })
    ));
    assert!(matches!(
        &entries[1],
        Entry::Script(FileEntry {
            mode: Some(0o750),
            ..
        })
    ));
}

#[test]
fn sudo_entry_requires_root_ownership() {
    let temp = TempDir::new();
    if fs::metadata(temp.path()).unwrap().uid() == 0 {
        return;
    }
    let mut entry = config_entry(temp.path(), None);
    let Entry::Config(file) = &mut entry else {
        unreachable!();
    };
    file.sudo = true;
    fs::create_dir_all(entry.target().parent().unwrap()).unwrap();
    fs::copy(entry.source(), entry.target()).unwrap();
    fs::set_permissions(entry.target(), fs::Permissions::from_mode(0o644)).unwrap();

    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Modified);
}

#[test]
fn reports_unreadable_file_without_failing_status() {
    let temp = TempDir::new();
    if fs::metadata(temp.path()).unwrap().uid() == 0 {
        return;
    }
    let entry = config_entry(temp.path(), Some(0o000));
    install(&entry, false).unwrap();

    assert_eq!(entry_status(&entry).unwrap(), EntryStatus::Unreadable);
}
