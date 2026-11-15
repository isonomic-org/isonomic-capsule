//! Putting one verified download in place, by install shape.
//!
//! - `directory`: unpack, keep the tree under `pathInArchive`, trim `bin/`.
//! - `file`: unpack and keep one program (or take the download itself when
//!   the archive is `none`).
//! - `build`: unpack the source, run the pinned command for this target, and
//!   keep the program it produces. The build log is kept next to the cache,
//!   whether the build passes or not.
//!
//! The caller has already checked the hash; it stamps and architecture-checks
//! the result afterwards.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::install::{contents, InstallError};
use crate::manifest::{Sidecar, Target};

/// The architecture name compilers and `lipo` use for a target triple.
pub fn arch_name(triple: &str) -> &str {
    match triple.split('-').next().unwrap_or(triple) {
        "aarch64" if triple.ends_with("apple-darwin") => "arm64",
        other => other,
    }
}

pub(crate) fn place(
    sidecar: &Sidecar,
    target: &Target,
    triple: &str,
    label: &str,
    payload: &[u8],
    work: &Path,
    dest: &Path,
) -> Result<(), InstallError> {
    let archive_error = |message: String| InstallError::Archive(label.to_string(), message);
    if target.archive == "none" {
        std::fs::write(work.join(&sidecar.name), payload)?;
    } else {
        tar::Archive::new(flate2::read::GzDecoder::new(payload))
            .unpack(work)
            .map_err(|error| archive_error(format!("cannot unpack: {error}")))?;
    }
    let wanted = match target.archive.as_str() {
        "none" => sidecar.name.clone(),
        _ => target.path_in_archive.clone().unwrap_or_default(),
    };
    let member = work.join(&wanted);
    let program = sidecar.program(target);

    if sidecar.is_directory() {
        return place_directory(sidecar, &member, &wanted, &program, dest, label);
    }
    let produced = match &sidecar.build {
        Some(build) => {
            if !member.is_dir() {
                let found = contents(work);
                return Err(archive_error(format!(
                    "pathInArchive \"{wanted}\" is not the source directory. Archive contains:\n  {found}"
                )));
            }
            let log = work.with_file_name(format!("{}-{triple}.build.log", sidecar.name));
            build_from_source(build, &member, triple, &log, label)?
        }
        None => member,
    };
    if !produced.is_file() {
        let found = contents(work);
        return Err(archive_error(format!(
            "\"{}\" is not a file. Found:\n  {found}",
            produced.strip_prefix(work).unwrap_or(&produced).display()
        )));
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest)?;
    std::fs::copy(&produced, dest.join(&program))?;
    make_executable(&dest.join(&program))
}

fn place_directory(
    sidecar: &Sidecar,
    member: &Path,
    wanted: &str,
    program: &str,
    dest: &Path,
    label: &str,
) -> Result<(), InstallError> {
    let archive_error = |message: String| InstallError::Archive(label.to_string(), message);
    if !member.is_dir() {
        let found = contents(member.parent().unwrap_or(member));
        return Err(archive_error(format!(
            "pathInArchive \"{wanted}\" is not a directory. Archive contains:\n  {found}"
        )));
    }
    if !member.join(program).is_file() {
        let found = contents(member);
        return Err(archive_error(format!(
            "binaryInDir \"{program}\" not found. Directory contains:\n  {found}"
        )));
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::rename(member, dest)?;

    // Bundle only the programs the engine runs.
    if let Some(keep) = &sidecar.keep_in_bin {
        for entry in std::fs::read_dir(dest.join("bin"))?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !keep.contains(&name) {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    make_executable(&dest.join(program))
}

/// The pinned build: `command` with `args` in the source directory, `env`
/// added to the environment, `{arch}` replaced by this target's architecture.
fn build_from_source(
    build: &Value,
    source: &Path,
    triple: &str,
    log: &Path,
    label: &str,
) -> Result<PathBuf, InstallError> {
    let arch = arch_name(triple);
    let fill = |text: &str| text.replace("{arch}", arch);
    let field = |key: &str| build.get(key).and_then(Value::as_str).map(fill);
    let command = field("command").unwrap_or_default();
    let produces = field("producesPath").unwrap_or_default();
    let args: Vec<String> = build
        .get("args")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).map(fill).collect())
        .unwrap_or_default();
    let env: Vec<(String, String)> = build
        .get("env")
        .and_then(Value::as_object)
        .map(|env| {
            env.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), fill(v))))
                .collect()
        })
        .unwrap_or_default();

    let out = std::fs::File::create(log)?;
    let err = out.try_clone()?;
    let status = Command::new(&command)
        .args(&args)
        .envs(env)
        .current_dir(source)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|error| InstallError::Build {
            label: label.to_string(),
            status: format!("cannot start {command}: {error}"),
            log: log.to_path_buf(),
            tail: String::new(),
        })?;
    if !status.success() {
        let text = std::fs::read_to_string(log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let tail = lines[lines.len().saturating_sub(20)..].join("\n");
        return Err(InstallError::Build {
            label: label.to_string(),
            status: status.to_string(),
            log: log.to_path_buf(),
            tail,
        });
    }
    Ok(source.join(produces))
}

fn make_executable(path: &Path) -> Result<(), InstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
#[path = "place_tests.rs"]
mod tests;
