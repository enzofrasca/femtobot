use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone)]
pub(super) struct ParsedSource {
    pub(super) original: String,
    pub(super) git_url: Option<String>,
    pub(super) local_path: Option<PathBuf>,
    pub(super) ref_name: Option<String>,
    pub(super) subpath: Option<PathBuf>,
    pub(super) skill_filter: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct DiscoveredSkill {
    pub(super) dir: PathBuf,
    pub(super) name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct SkillFrontmatter {
    pub(super) name: Option<String>,
}

pub(super) fn parse_source(source: &str) -> Result<ParsedSource> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        bail!("source cannot be empty");
    }

    if is_local_path(trimmed) {
        let local_path = PathBuf::from(trimmed);
        return Ok(ParsedSource {
            original: trimmed.to_string(),
            git_url: None,
            local_path: Some(local_path),
            ref_name: None,
            subpath: None,
            skill_filter: None,
        });
    }

    if let Some(parsed) = parse_owner_repo_source(trimmed)? {
        return Ok(parsed);
    }

    if let Some(parsed) = parse_github_url_source(trimmed)? {
        return Ok(parsed);
    }

    Ok(ParsedSource {
        original: trimmed.to_string(),
        git_url: Some(trimmed.to_string()),
        local_path: None,
        ref_name: None,
        subpath: None,
        skill_filter: None,
    })
}

fn parse_owner_repo_source(source: &str) -> Result<Option<ParsedSource>> {
    let looks_like_url = source.contains("://") || source.starts_with("git@");
    if looks_like_url {
        return Ok(None);
    }

    let mut repo_and_path = source;
    let mut skill_filter = None;
    if let Some(index) = source.rfind('@') {
        if index > 0 {
            repo_and_path = &source[..index];
            let raw = source[index + 1..].trim();
            if !raw.is_empty() {
                skill_filter = Some(raw.to_string());
            }
        }
    }

    let segments: Vec<&str> = repo_and_path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    if segments.len() < 2 {
        return Ok(None);
    }

    let owner = segments[0];
    let repo = segments[1];
    if owner.starts_with('.') || repo.starts_with('.') || owner.contains('.') {
        return Ok(None);
    }

    let subpath = if segments.len() > 2 {
        let joined = segments[2..].join("/");
        Some(PathBuf::from(joined))
    } else {
        None
    };

    Ok(Some(ParsedSource {
        original: source.to_string(),
        git_url: Some(format!("https://github.com/{owner}/{repo}.git")),
        local_path: None,
        ref_name: None,
        subpath,
        skill_filter,
    }))
}

fn parse_github_url_source(source: &str) -> Result<Option<ParsedSource>> {
    if !(source.starts_with("https://github.com/") || source.starts_with("http://github.com/")) {
        return Ok(None);
    }

    let url = Url::parse(source).with_context(|| format!("invalid source URL: {}", source))?;
    let mut segments = url
        .path_segments()
        .ok_or_else(|| anyhow!("invalid GitHub URL path"))?
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    if segments.len() < 2 {
        return Ok(None);
    }

    let owner = segments.remove(0).to_string();
    let mut repo = segments.remove(0).to_string();
    repo = repo.trim_end_matches(".git").to_string();

    let mut ref_name = None;
    let mut subpath = None;

    if segments.first().copied() == Some("tree") && segments.len() >= 2 {
        ref_name = Some(segments[1].to_string());
        if segments.len() > 2 {
            subpath = Some(PathBuf::from(segments[2..].join("/")));
        }
    }

    Ok(Some(ParsedSource {
        original: source.to_string(),
        git_url: Some(format!("https://github.com/{owner}/{repo}.git")),
        local_path: None,
        ref_name,
        subpath,
        skill_filter: None,
    }))
}

fn is_local_path(input: &str) -> bool {
    let path = Path::new(input);
    path.is_absolute()
        || input.starts_with("./")
        || input.starts_with("../")
        || input == "."
        || input == ".."
}

pub(super) fn clone_repo(parsed: &ParsedSource, clone_dir: &Path) -> Result<()> {
    let Some(git_url) = parsed.git_url.as_ref() else {
        bail!("missing git URL for clone");
    };

    let mut command = Command::new("git");
    command.arg("clone").arg("--depth").arg("1");
    if let Some(ref_name) = parsed.ref_name.as_deref() {
        command.arg("--branch").arg(ref_name);
    }
    command.arg(git_url).arg(clone_dir);

    let output = command
        .output()
        .with_context(|| format!("failed to execute git clone for {}", git_url))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let details = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            "unknown git error".to_string()
        };
        bail!("git clone failed for {}: {}", git_url, details);
    }

    Ok(())
}

pub(super) fn discover_skills(root: &Path) -> Result<Vec<DiscoveredSkill>> {
    let mut found = Vec::new();
    let mut seen_dirs = HashSet::new();

    if root.join(super::SKILL_FILE_NAME).is_file() {
        found.push(DiscoveredSkill {
            dir: root.to_path_buf(),
            name: read_skill_name(&root.join(super::SKILL_FILE_NAME))?,
        });
        seen_dirs.insert(root.to_path_buf());
    }

    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(should_descend_into_dir);

    for entry in walker {
        let entry = entry.with_context(|| format!("failed to walk {}", root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        if entry.file_name() != super::SKILL_FILE_NAME {
            continue;
        }

        let Some(parent) = entry.path().parent() else {
            continue;
        };
        let parent_path = parent.to_path_buf();
        if seen_dirs.contains(&parent_path) {
            continue;
        }
        seen_dirs.insert(parent_path.clone());

        found.push(DiscoveredSkill {
            dir: parent_path,
            name: read_skill_name(entry.path())?,
        });
    }

    Ok(found)
}

fn should_descend_into_dir(entry: &DirEntry) -> bool {
    if !entry.file_type().is_dir() {
        return true;
    }

    let name = entry.file_name().to_string_lossy();
    !matches!(
        name.as_ref(),
        ".git" | "node_modules" | "dist" | "build" | "__pycache__" | "target" | ".venv" | "venv"
    )
}

fn read_skill_name(skill_md_path: &Path) -> Result<Option<String>> {
    let content = fs::read_to_string(skill_md_path)
        .with_context(|| format!("failed to read {}", skill_md_path.display()))?;
    Ok(parse_frontmatter(&content).and_then(|fm| fm.name.map(|name| name.trim().to_string())))
}

pub(super) fn parse_frontmatter(markdown: &str) -> Option<SkillFrontmatter> {
    let mut lines = markdown.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let mut yaml = String::new();
    let mut saw_close = false;
    for line in lines {
        if line.trim() == "---" {
            saw_close = true;
            break;
        }
        yaml.push_str(line);
        yaml.push('\n');
    }

    if !saw_close || yaml.trim().is_empty() {
        return None;
    }

    serde_yaml::from_str::<SkillFrontmatter>(&yaml).ok()
}

pub(super) fn filter_discovered_skills(
    skills: Vec<DiscoveredSkill>,
    filters: &[String],
) -> Vec<DiscoveredSkill> {
    let normalized_filters = normalize_filters(filters);
    if normalized_filters.is_empty() || normalized_filters.iter().any(|f| f == "*") {
        return skills;
    }

    skills
        .into_iter()
        .filter(|skill| {
            let dir_name = skill
                .dir
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let skill_name = skill
                .name
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();

            normalized_filters
                .iter()
                .any(|needle| needle == &dir_name || needle == &skill_name)
        })
        .collect()
}

pub(super) fn normalize_filters(filters: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for raw in filters {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let extracted = trimmed
            .rsplit_once('@')
            .map(|(_, skill)| skill)
            .unwrap_or(trimmed);
        out.push(extracted.trim().to_ascii_lowercase());
    }
    out
}

pub(super) fn pick_unique_install_name(base: &str, used_names: &mut HashSet<String>) -> String {
    if used_names.insert(base.to_string()) {
        return base.to_string();
    }

    let mut index = 2usize;
    loop {
        let candidate = format!("{base}-{index}");
        if used_names.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}
