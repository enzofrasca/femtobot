use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Component, Path, PathBuf};
use walkdir::WalkDir;
use zip::ZipArchive;

pub(super) fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory: {}", path.display()))
}

pub(super) fn prepare_install_target(path: &Path, force: bool) -> Result<()> {
    if path.exists() {
        if !force {
            bail!(
                "target already exists: {} (set force=true to overwrite)",
                path.display()
            );
        }
        fs::remove_dir_all(path)
            .with_context(|| format!("failed to remove existing target: {}", path.display()))?;
    }
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create target directory: {}", path.display()))
}

pub(super) fn extract_zip_to_dir(zip_bytes: &[u8], target_dir: &Path) -> Result<()> {
    let mut archive =
        ZipArchive::new(Cursor::new(zip_bytes)).context("failed to open zip archive")?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .with_context(|| format!("failed to read zip entry index {}", index))?;

        let Some(rel_path) = entry.enclosed_name().map(PathBuf::from) else {
            continue;
        };

        let out_path = target_dir.join(rel_path);
        if entry.is_dir() || entry.name().ends_with('/') {
            fs::create_dir_all(&out_path)
                .with_context(|| format!("failed to create directory: {}", out_path.display()))?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory: {}", parent.display())
            })?;
        }

        let mut out_file = fs::File::create(&out_path)
            .with_context(|| format!("failed to create file: {}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out_file)
            .with_context(|| format!("failed to extract file: {}", out_path.display()))?;
        out_file
            .flush()
            .with_context(|| format!("failed to flush file: {}", out_path.display()))?;
    }

    Ok(())
}

pub(super) fn maybe_flatten_single_nested_skill_dir(target_dir: &Path) -> Result<()> {
    if target_dir.join(super::SKILL_FILE_NAME).is_file() {
        return Ok(());
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in fs::read_dir(target_dir)
        .with_context(|| format!("failed to read directory: {}", target_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", target_dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.is_file() {
            files.push(path);
        }
    }

    if files.is_empty() && dirs.len() == 1 {
        let child = &dirs[0];
        if child.join(super::SKILL_FILE_NAME).is_file() {
            for entry in fs::read_dir(child)
                .with_context(|| format!("failed to read nested dir: {}", child.display()))?
            {
                let entry = entry.with_context(|| {
                    format!("failed to read nested entry in {}", child.display())
                })?;
                let from = entry.path();
                let to = target_dir.join(entry.file_name());
                fs::rename(&from, &to).with_context(|| {
                    format!(
                        "failed to move extracted content from {} to {}",
                        from.display(),
                        to.display()
                    )
                })?;
            }
            fs::remove_dir_all(child)
                .with_context(|| format!("failed to remove nested dir: {}", child.display()))?;
        }
    }

    Ok(())
}

pub(super) fn ensure_skill_md_exists(dir: &Path) -> Result<()> {
    let skill_md = dir.join(super::SKILL_FILE_NAME);
    if skill_md.is_file() {
        return Ok(());
    }
    bail!(
        "installed directory is missing {} at root: {}",
        super::SKILL_FILE_NAME,
        dir.display()
    )
}

pub(super) fn copy_directory(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create target directory: {}", target.display()))?;

    for entry in WalkDir::new(source).follow_links(false).into_iter() {
        let entry = entry.with_context(|| format!("failed to walk {}", source.display()))?;
        let path = entry.path();
        if path == source {
            continue;
        }

        let relative = path
            .strip_prefix(source)
            .with_context(|| format!("failed to compute relative path for {}", path.display()))?;

        if !is_safe_relative_path(relative) {
            bail!("unsafe relative path while copying: {}", relative.display());
        }

        let destination = target.join(relative);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            fs::create_dir_all(&destination).with_context(|| {
                format!(
                    "failed to create destination directory: {}",
                    destination.display()
                )
            })?;
            continue;
        }
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_file() {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create destination parent: {}", parent.display())
                })?;
            }
            fs::copy(path, &destination).with_context(|| {
                format!(
                    "failed to copy file from {} to {}",
                    path.display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

pub fn sanitize_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let mut sanitized = String::with_capacity(lower.len());
    let mut previous_dash = false;

    for ch in lower.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' {
            sanitized.push(ch);
            previous_dash = false;
            continue;
        }

        if !previous_dash {
            sanitized.push('-');
            previous_dash = true;
        }
    }

    while sanitized.starts_with(['.', '-']) {
        sanitized.remove(0);
    }
    while sanitized.ends_with(['.', '-']) {
        sanitized.pop();
    }

    if sanitized.is_empty() {
        "unnamed-skill".to_string()
    } else {
        sanitized.chars().take(255).collect()
    }
}
