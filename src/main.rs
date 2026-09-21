use magsys::{
    EntryStatus, Error, InstallOptions, ensure_not_running_as_root, entry_status,
    find_config_upwards, install_entry, load_entries,
};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("magsys: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        print_usage();
        return Ok(2);
    };

    let mut config: Option<PathBuf> = None;
    let mut dry_run = false;
    let mut force = false;
    let remaining: Vec<String> = args.collect();
    let mut index = 0;
    while index < remaining.len() {
        match remaining[index].as_str() {
            "--config" | "-c" => {
                index += 1;
                let value = remaining.get(index).ok_or("--config requires a path")?;
                config = Some(PathBuf::from(value));
            }
            "--dry-run" if command == "install" => dry_run = true,
            "--force" if command == "install" => force = true,
            "--help" | "-h" => {
                print_usage();
                return Ok(0);
            }
            value => return Err(format!("unknown argument: {value}").into()),
        }
        index += 1;
    }

    if matches!(command.as_str(), "help" | "--help" | "-h") {
        print_usage();
        return Ok(0);
    }
    if command != "status" && command != "install" {
        return Err(format!("unknown command: {command}").into());
    }

    let config = match config {
        Some(path) => path,
        None => find_config_upwards(&env::current_dir()?)?,
    };
    let entries = load_entries(&config)?;
    match command.as_str() {
        "status" => {
            let mut clean = true;
            for entry in &entries {
                let status = entry_status(entry)?;
                clean &= status == EntryStatus::Ok;
                println!("{status:<8} {}", entry.target().display());
            }
            Ok(if clean { 0 } else { 1 })
        }
        "install" => {
            ensure_not_running_as_root()?;
            for entry in &entries {
                if entry_status(entry)? == EntryStatus::Conflict && !force {
                    return Err(Box::new(Error::Conflict(entry.target().to_path_buf())));
                }
            }
            for entry in &entries {
                for operation in install_entry(entry, InstallOptions { dry_run, force })? {
                    if dry_run {
                        println!("would {operation}");
                    } else {
                        println!("{operation}");
                    }
                }
            }
            Ok(0)
        }
        value => Err(format!("unknown command: {value}").into()),
    }
}

fn print_usage() {
    println!(
        "Usage:\n  magsys status [--config PATH]\n  magsys install [--dry-run] [--force] [--config PATH]"
    );
}
