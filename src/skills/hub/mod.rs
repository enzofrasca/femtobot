use anyhow::{anyhow, bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::Url;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

mod http;
mod install;
mod source;

pub use install::sanitize_name;

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
                pairs.append_pair("limit", &http::normalize_limit(limit).to_string());
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
                pairs.append_pair("limit", &http::normalize_limit(limit).to_string());
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
        install::ensure_dir(&request.skills_root)?;

        let install_name = sanitize_name(slug);
        let target_dir = request.skills_root.join(&install_name);
        install::prepare_install_target(&target_dir, request.force)?;

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
        install::extract_zip_to_dir(&zip_bytes, &target_dir)?;
        install::maybe_flatten_single_nested_skill_dir(&target_dir)?;
        install::ensure_skill_md_exists(&target_dir)?;

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
        let parsed = source::parse_source(&request.source)?;
        install::ensure_dir(&request.skills_root)?;

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
            source::clone_repo(&parsed, temp.path())?;
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

        let discovered = source::discover_skills(&search_root)?;
        if discovered.is_empty() {
            bail!("no SKILL.md files found in source: {}", request.source);
        }

        let selected = source::filter_discovered_skills(discovered, &merged_filters);
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

            let install_name = source::pick_unique_install_name(&base_name, &mut used_names);
            let target_dir = request.skills_root.join(&install_name);
            install::prepare_install_target(&target_dir, request.force)?;
            install::copy_directory(&skill.dir, &target_dir)?;
            install::ensure_skill_md_exists(&target_dir)?;

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
        let parsed = source::parse_source(source)?;

        let temp_guard = if parsed.local_path.is_some() {
            None
        } else {
            Some(tempdir().context("failed to create temp dir for git clone")?)
        };

        let source_root = if let Some(local_path) = parsed.local_path.as_ref() {
            local_path.clone()
        } else if let Some(temp) = temp_guard.as_ref() {
            source::clone_repo(&parsed, temp.path())?;
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

        let mut discovered = source::discover_skills(&search_root)?;
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
        http::parse_json_response(response, &url)
    }

    fn get_bytes(&self, url: Url) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .with_context(|| format!("GET request failed: {}", url))?;
        let checked = http::ensure_success(response, &url)?;
        let bytes = checked
            .bytes()
            .with_context(|| format!("failed to read response bytes: {}", url))?;
        Ok(bytes.to_vec())
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
        let parsed = source::parse_frontmatter(markdown).expect("frontmatter should parse");
        assert_eq!(parsed.name.as_deref(), Some("Demo Skill"));
    }

    #[test]
    fn parse_source_supports_owner_repo_with_filter() {
        let parsed =
            source::parse_source("vercel-labs/agent-skills@web-design").expect("should parse");
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
        let normalized = source::normalize_filters(&filters);
        assert_eq!(normalized, vec!["my skill", "frontend"]);
    }
}
