use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::{Client, Response};
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::io::{Cursor, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;
use walkdir::{DirEntry, WalkDir};
use zip::ZipArchive;

pub const DEFAULT_CLAWHUB_BASE_URL: &str = "https://clawhub.ai";
pub const DEFAULT_SKILLS_SH_BASE_URL: &str = "https://skills.sh";

const REQUEST_TIMEOUT_SECS: u64 = 20;
const SKILL_FILE_NAME: &str = "SKILL.md";
const MAX_SEARCH_LIMIT: usize = 100;

#[derive(Debug, Clone)]
pub struct Skillhub {
    client: Client,
    clawhub_base_url: String,
    skills_sh_base_url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClawhubSearchResult {
    #[serde(default)]
    pub slug: String,
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub score: f64,
    #[serde(default, rename = "updatedAt")]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillsShSearchResult {
    #[serde(default, rename = "id")]
    pub slug: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub installs: u64,
}

#[derive(Debug, Clone)]
pub struct ClawhubInstallRequest {
    pub slug: String,
    pub version: Option<String>,
    pub tag: Option<String>,
    pub skills_root: PathBuf,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct SkillsSourceInstallRequest {
    pub source: String,
    pub skill_filters: Vec<String>,
    pub skills_root: PathBuf,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct SkillsShInstallRequest {
    pub slug_or_query: String,
    pub skills_root: PathBuf,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct InstalledSkill {
    pub install_name: String,
    pub path: PathBuf,
    pub source: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SourceSkill {
    pub directory: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
struct ParsedSource {
    original: String,
    git_url: Option<String>,
    local_path: Option<PathBuf>,
    ref_name: Option<String>,
    subpath: Option<PathBuf>,
    skill_filter: Option<String>,
}

#[derive(Debug, Clone)]
struct DiscoveredSkill {
    dir: PathBuf,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClawhubSearchResponse {
    #[serde(default)]
    results: Vec<ClawhubSearchResult>,
}

#[derive(Debug, Deserialize)]
struct SkillsShSearchResponse {
    #[serde(default)]
    skills: Vec<SkillsShSearchResult>,
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
}

impl Skillhub {
    pub fn new() -> Result<Self> {
        Self::with_bases(DEFAULT_CLAWHUB_BASE_URL, DEFAULT_SKILLS_SH_BASE_URL)
    }

    pub fn with_bases(clawhub_base_url: &str, skills_sh_base_url: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            clawhub_base_url: clawhub_base_url.trim_end_matches('/').to_string(),
            skills_sh_base_url: skills_sh_base_url.trim_end_matches('/').to_string(),
        })
    }

    pub fn search_clawhub(&self, query: &str, limit: usize) -> Result<Vec<ClawhubSearchResult>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            bail!("query cannot be empty");
        }

        let mut url = Url::parse(&format!("{}/api/v1/search", self.clawhub_base_url))
            .context("invalid ClawHub base URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", trimmed);
            if limit > 0 {
                pairs.append_pair("limit", &normalize_limit(limit).to_string());
            }
        }

        let response: ClawhubSearchResponse = self.get_json(url)?;
        Ok(response.results)
    }

    pub fn search_skills_sh(&self, query: &str, limit: usize) -> Result<Vec<SkillsShSearchResult>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            bail!("query cannot be empty");
        }

        let mut url = Url::parse(&format!("{}/api/search", self.skills_sh_base_url))
            .context("invalid skills.sh base URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("q", trimmed);
            if limit > 0 {
                pairs.append_pair("limit", &normalize_limit(limit).to_string());
            }
        }

        let response: SkillsShSearchResponse = self.get_json(url)?;
        Ok(response.skills)
    }

    pub fn install_from_clawhub(&self, request: ClawhubInstallRequest) -> Result<InstalledSkill> {
        let slug = request.slug.trim();
        if slug.is_empty() {
            bail!("slug cannot be empty");
        }
        ensure_dir(&request.skills_root)?;

        let install_name = sanitize_name(slug);
        let target_dir = request.skills_root.join(&install_name);
        prepare_install_target(&target_dir, request.force)?;

        let mut url = Url::parse(&format!("{}/api/v1/download", self.clawhub_base_url))
            .context("invalid ClawHub base URL")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("slug", slug);
            if let Some(version) = request
                .version
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("version", version);
            }
            if let Some(tag) = request
                .tag
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                pairs.append_pair("tag", tag);
            }
        }

        let zip_bytes = self.get_bytes(url)?;
        extract_zip_to_dir(&zip_bytes, &target_dir)?;
        maybe_flatten_single_nested_skill_dir(&target_dir)?;
        ensure_skill_md_exists(&target_dir)?;

        Ok(InstalledSkill {
            install_name,
            path: target_dir,
            source: format!("clawhub:{}", slug),
            version: request.version,
        })
    }

    pub fn install_from_skills_source(
        &self,
        request: SkillsSourceInstallRequest,
    ) -> Result<Vec<InstalledSkill>> {
        let parsed = parse_source(&request.source)?;
        ensure_dir(&request.skills_root)?;

        let extra_filters: Vec<String> = request
            .skill_filters
            .into_iter()
            .map(|f| f.trim().to_string())
            .filter(|f| !f.is_empty())
            .collect();

        let mut merged_filters = extra_filters;
        if let Some(filter) = parsed.skill_filter.as_ref() {
            merged_filters.push(filter.clone());
        }

        let temp_guard = if parsed.local_path.is_some() {
            None
        } else {
            Some(tempdir().context("failed to create temp dir for git clone")?)
        };

        let source_root = if let Some(local_path) = parsed.local_path.as_ref() {
            local_path.clone()
        } else if let Some(temp) = temp_guard.as_ref() {
            clone_repo(&parsed, temp.path())?;
            temp.path().to_path_buf()
        } else {
            bail!("failed to prepare source directory");
        };

        let search_root = if let Some(subpath) = parsed.subpath.as_ref() {
            source_root.join(subpath)
        } else {
            source_root
        };
        if !search_root.exists() {
            bail!("source subpath does not exist: {}", search_root.display());
        }

        let discovered = discover_skills(&search_root)?;
        if discovered.is_empty() {
            bail!("no SKILL.md files found in source: {}", request.source);
        }

        let selected = filter_discovered_skills(discovered, &merged_filters);
        if selected.is_empty() {
            bail!("no skills matched filters: {}", merged_filters.join(", "));
        }

        let mut installed = Vec::new();
        let mut used_names = HashSet::new();
        for skill in selected {
            let base_name = skill
                .name
                .as_deref()
                .map(sanitize_name)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    let fallback = skill
                        .dir
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("skill");
                    sanitize_name(fallback)
                });

            let install_name = pick_unique_install_name(&base_name, &mut used_names);
            let target_dir = request.skills_root.join(&install_name);
            prepare_install_target(&target_dir, request.force)?;
            copy_directory(&skill.dir, &target_dir)?;
            ensure_skill_md_exists(&target_dir)?;

            installed.push(InstalledSkill {
                install_name,
                path: target_dir,
                source: parsed.original.clone(),
                version: None,
            });
        }

        Ok(installed)
    }

    pub fn list_from_skills_source(&self, source: &str) -> Result<Vec<SourceSkill>> {
        let parsed = parse_source(source)?;

        let temp_guard = if parsed.local_path.is_some() {
            None
        } else {
            Some(tempdir().context("failed to create temp dir for git clone")?)
        };

        let source_root = if let Some(local_path) = parsed.local_path.as_ref() {
            local_path.clone()
        } else if let Some(temp) = temp_guard.as_ref() {
            clone_repo(&parsed, temp.path())?;
            temp.path().to_path_buf()
        } else {
            bail!("failed to prepare source directory");
        };

        let search_root = if let Some(subpath) = parsed.subpath.as_ref() {
            source_root.join(subpath)
        } else {
            source_root
        };
        if !search_root.exists() {
            bail!("source subpath does not exist: {}", search_root.display());
        }

        let mut discovered = discover_skills(&search_root)?;
        if discovered.is_empty() {
            bail!("no SKILL.md files found in source: {}", source);
        }

        discovered.sort_by(|a, b| {
            let a_name = a.name.as_deref().unwrap_or("");
            let b_name = b.name.as_deref().unwrap_or("");
            a_name
                .cmp(b_name)
                .then_with(|| a.dir.as_os_str().cmp(b.dir.as_os_str()))
        });

        Ok(discovered
            .into_iter()
            .map(|entry| SourceSkill {
                directory: entry
                    .dir
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("")
                    .to_string(),
                name: entry.name,
            })
            .collect())
    }

    pub fn install_from_skills_sh(
        &self,
        request: SkillsShInstallRequest,
    ) -> Result<Vec<InstalledSkill>> {
        let query = request.slug_or_query.trim();
        if query.is_empty() {
            bail!("slug_or_query cannot be empty");
        }

        let results = self.search_skills_sh(query, 25)?;
        if results.is_empty() {
            bail!("no skills.sh results found for query: {}", query);
        }

        let selected = results
            .iter()
            .find(|entry| entry.slug.eq_ignore_ascii_case(query))
            .or_else(|| {
                results
                    .iter()
                    .find(|entry| entry.name.eq_ignore_ascii_case(query))
            })
            .unwrap_or(&results[0]);

        let source = if selected.source.trim().is_empty() {
            selected.slug.clone()
        } else {
            selected.source.clone()
        };

        self.install_from_skills_source(SkillsSourceInstallRequest {
            source,
            skill_filters: vec![selected.name.clone()],
            skills_root: request.skills_root,
            force: request.force,
        })
    }

    fn get_json<T: DeserializeOwned>(&self, url: Url) -> Result<T> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .with_context(|| format!("GET request failed: {}", url))?;
        parse_json_response(response, &url)
    }

    fn get_bytes(&self, url: Url) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .with_context(|| format!("GET request failed: {}", url))?;
        let checked = ensure_success(response, &url)?;
        let bytes = checked
            .bytes()
            .with_context(|| format!("failed to read response bytes: {}", url))?;
        Ok(bytes.to_vec())
    }
}

fn normalize_limit(limit: usize) -> usize {
    limit.clamp(1, MAX_SEARCH_LIMIT)
}

fn parse_json_response<T: DeserializeOwned>(response: Response, url: &Url) -> Result<T> {
    let checked = ensure_success(response, url)?;
    checked
        .json::<T>()
        .with_context(|| format!("failed to parse JSON response: {}", url))
}

fn ensure_success(response: Response, url: &Url) -> Result<Response> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let body = response.text().unwrap_or_else(|_| String::new());
    let snippet = body.trim();
    if snippet.is_empty() {
        bail!("request failed ({}): {}", status, url);
    }
    bail!("request failed ({}): {} -> {}", status, url, snippet)
}

fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create directory: {}", path.display()))
}

fn prepare_install_target(path: &Path, force: bool) -> Result<()> {
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

fn extract_zip_to_dir(zip_bytes: &[u8], target_dir: &Path) -> Result<()> {
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

fn maybe_flatten_single_nested_skill_dir(target_dir: &Path) -> Result<()> {
    if target_dir.join(SKILL_FILE_NAME).is_file() {
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
        if child.join(SKILL_FILE_NAME).is_file() {
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

fn ensure_skill_md_exists(dir: &Path) -> Result<()> {
    let skill_md = dir.join(SKILL_FILE_NAME);
    if skill_md.is_file() {
        return Ok(());
    }
    bail!(
        "installed directory is missing {} at root: {}",
        SKILL_FILE_NAME,
        dir.display()
    )
}

fn parse_source(source: &str) -> Result<ParsedSource> {
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

fn clone_repo(parsed: &ParsedSource, clone_dir: &Path) -> Result<()> {
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

fn discover_skills(root: &Path) -> Result<Vec<DiscoveredSkill>> {
    let mut found = Vec::new();
    let mut seen_dirs = HashSet::new();

    if root.join(SKILL_FILE_NAME).is_file() {
        found.push(DiscoveredSkill {
            dir: root.to_path_buf(),
            name: read_skill_name(&root.join(SKILL_FILE_NAME))?,
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
        if entry.file_name() != SKILL_FILE_NAME {
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

fn parse_frontmatter(markdown: &str) -> Option<SkillFrontmatter> {
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

fn filter_discovered_skills(
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

fn normalize_filters(filters: &[String]) -> Vec<String> {
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

fn pick_unique_install_name(base: &str, used_names: &mut HashSet<String>) -> String {
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

fn copy_directory(source: &Path, target: &Path) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name_normalizes_and_trims() {
        assert_eq!(sanitize_name("  Hello World!  "), "hello-world");
        assert_eq!(sanitize_name("../My_Skill"), "my_skill");
        assert_eq!(sanitize_name("..."), "unnamed-skill");
    }

    #[test]
    fn parse_frontmatter_reads_name() {
        let markdown = r#"---
name: Demo Skill
description: Example
---
Body
"#;
        let parsed = parse_frontmatter(markdown).expect("frontmatter should parse");
        assert_eq!(parsed.name.as_deref(), Some("Demo Skill"));
    }

    #[test]
    fn parse_source_supports_owner_repo_with_filter() {
        let parsed = parse_source("vercel-labs/agent-skills@web-design").expect("should parse");
        assert_eq!(
            parsed.git_url.as_deref(),
            Some("https://github.com/vercel-labs/agent-skills.git")
        );
        assert_eq!(parsed.skill_filter.as_deref(), Some("web-design"));
        assert!(parsed.subpath.is_none());
    }

    #[test]
    fn normalize_filters_extracts_at_suffix() {
        let filters = vec![
            "owner/repo@My Skill".to_string(),
            "Frontend".to_string(),
            " ".to_string(),
        ];
        let normalized = normalize_filters(&filters);
        assert_eq!(normalized, vec!["my skill", "frontend"]);
    }
}
