use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub dir_path: PathBuf,
    pub platforms: Vec<String>,
    pub deps: Vec<String>,
    pub source: String,
    pub version: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone)]
struct SkillRoot {
    path: PathBuf,
    source: String,
}

#[derive(Debug, Deserialize, Default)]
struct SkillFrontmatter {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    compatibility: SkillCompatibility,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct SkillCompatibility {
    #[serde(default)]
    os: Vec<String>,
    #[serde(default)]
    deps: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SkillManager {
    roots: Vec<SkillRoot>,
}

impl SkillManager {
    pub fn from_workspace_dir(workspace_dir: &Path) -> Self {
        let mut roots = Vec::new();
        if let Some(home) = dirs::home_dir() {
            roots.push(SkillRoot {
                path: home.join(".agents").join("skills"),
                source: "agents-personal".to_string(),
            });
        }
        roots.push(SkillRoot {
            path: workspace_dir.join(".agents").join("skills"),
            source: "agents-project".to_string(),
        });
        roots.push(SkillRoot {
            path: workspace_dir.join("skills"),
            source: "workspace".to_string(),
        });
        Self { roots }
    }

    pub fn discover_skills(&self) -> Vec<SkillMetadata> {
        self.discover_skills_internal(false)
    }

    fn discover_skills_internal(&self, include_unavailable: bool) -> Vec<SkillMetadata> {
        // Precedence is root-order based; later roots override earlier roots.
        let mut merged = HashMap::<String, SkillMetadata>::new();
        for root in &self.roots {
            let entries = match std::fs::read_dir(&root.path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let dir_path = entry.path();
                if !dir_path.is_dir() {
                    continue;
                }
                let skill_file = dir_path.join("SKILL.md");
                if !skill_file.exists() {
                    continue;
                }
                let content = match std::fs::read_to_string(&skill_file) {
                    Ok(content) => content,
                    Err(_) => continue,
                };
                let Some((meta, _body)) = parse_skill_md(&content, &dir_path, &root.source) else {
                    continue;
                };
                if include_unavailable || self.skill_is_available(&meta).is_ok() {
                    merged.insert(meta.name.to_ascii_lowercase(), meta);
                }
            }
        }

        let mut skills: Vec<SkillMetadata> = merged.into_values().collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    pub fn load_skill_checked(&self, name: &str) -> Result<(SkillMetadata, String), String> {
        let requested = name.trim();
        if requested.is_empty() {
            return Err("Skill name cannot be empty.".to_string());
        }

        let all_skills = self.discover_skills_internal(true);
        for skill in all_skills {
            if !skill.name.eq_ignore_ascii_case(requested) {
                continue;
            }

            self.skill_is_available(&skill)?;

            let skill_md = skill.dir_path.join("SKILL.md");
            if let Ok(content) = std::fs::read_to_string(&skill_md) {
                if let Some((meta, body)) = parse_skill_md(&content, &skill.dir_path, &skill.source)
                {
                    return Ok((meta, body));
                }
            }

            return Err(format!(
                "Skill '{requested}' exists but could not be loaded."
            ));
        }

        let available = self.discover_skills();
        if available.is_empty() {
            Err(format!(
                "Skill '{requested}' not found. No skills are currently available."
            ))
        } else {
            let names: Vec<&str> = available.iter().map(|s| s.name.as_str()).collect();
            Err(format!(
                "Skill '{requested}' not found. Available skills: {}",
                names.join(", ")
            ))
        }
    }

    fn skill_is_available(&self, skill: &SkillMetadata) -> Result<(), String> {
        if !platform_allowed(&skill.platforms) {
            return Err(format!(
                "Skill '{}' is not available on this platform (current: {}, supported: {}).",
                skill.name,
                current_platform(),
                skill.platforms.join(", ")
            ));
        }

        let missing = missing_deps(&skill.deps);
        if !missing.is_empty() {
            return Err(format!(
                "Skill '{}' is missing required dependencies: {}",
                skill.name,
                missing.join(", ")
            ));
        }

        Ok(())
    }

    pub fn build_skills_catalog(&self) -> String {
        let skills = self.discover_skills();
        if skills.is_empty() {
            return String::new();
        }
        let mut catalog = String::from("<available_skills>\n");
        for skill in skills {
            catalog.push_str(&format!("- {}: {}\n", skill.name, skill.description));
        }
        catalog.push_str("</available_skills>");
        catalog
    }
}

fn current_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

fn normalize_platform(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "macos" | "osx" => "darwin".to_string(),
        other => other.to_string(),
    }
}

fn platform_allowed(platforms: &[String]) -> bool {
    if platforms.is_empty() {
        return true;
    }

    let current = current_platform();
    platforms.iter().any(|platform| {
        let platform = normalize_platform(platform);
        platform == "all" || platform == "*" || platform == current
    })
}

fn command_exists(command: &str) -> bool {
    if command.trim().is_empty() {
        return true;
    }

    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::env::split_paths(&path_var);

    #[cfg(target_os = "windows")]
    let candidates: Vec<String> = {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        let ext_list: Vec<String> = exts
            .split(';')
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        let lower = command.to_ascii_lowercase();
        if ext_list.iter().any(|ext| lower.ends_with(ext)) {
            vec![command.to_string()]
        } else {
            let mut candidates = vec![command.to_string()];
            for ext in ext_list {
                candidates.push(format!("{command}{ext}"));
            }
            candidates
        }
    };

    #[cfg(not(target_os = "windows"))]
    let candidates: Vec<String> = vec![command.to_string()];

    for base in paths {
        for candidate in &candidates {
            if base.join(candidate).is_file() {
                return true;
            }
        }
    }
    false
}

fn missing_deps(deps: &[String]) -> Vec<String> {
    deps.iter()
        .filter(|dep| !command_exists(dep))
        .cloned()
        .collect()
}

fn parse_skill_md(
    content: &str,
    dir_path: &Path,
    default_source: &str,
) -> Option<(SkillMetadata, String)> {
    let trimmed = content.trim_start_matches('\u{feff}');
    let (yaml, body) = split_frontmatter(trimmed)?;
    if yaml.trim().is_empty() {
        return None;
    }

    let fm: SkillFrontmatter = serde_yaml::from_str(yaml).ok()?;

    let mut name = fm.name.unwrap_or_default().trim().to_string();
    if name.is_empty() {
        name = dir_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())?;
    }

    let mut platforms: Vec<String> = fm
        .platforms
        .into_iter()
        .chain(fm.compatibility.os)
        .map(|p| normalize_platform(&p))
        .filter(|p| !p.is_empty())
        .collect();
    platforms.sort();
    platforms.dedup();

    let mut deps: Vec<String> = fm
        .deps
        .into_iter()
        .chain(fm.compatibility.deps)
        .map(|dep| dep.trim().to_string())
        .filter(|dep| !dep.is_empty())
        .collect();
    deps.sort();
    deps.dedup();

    let source = fm
        .source
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_source.to_string());

    Some((
        SkillMetadata {
            name,
            description: fm.description,
            dir_path: dir_path.to_path_buf(),
            platforms,
            deps,
            source,
            version: fm
                .version
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            updated_at: fm
                .updated_at
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        },
        body,
    ))
}

fn split_frontmatter(content: &str) -> Option<(&str, String)> {
    let mut chunks = content.split_inclusive('\n');
    let first = chunks.next()?;
    if first.trim_end_matches(['\r', '\n']) != "---" {
        return None;
    }

    let mut consumed = first.len();
    let mut yaml_len = 0usize;
    for chunk in chunks {
        let trimmed = chunk.trim_end_matches(['\r', '\n']);
        consumed += chunk.len();
        if trimmed == "---" || trimmed == "..." {
            let yaml = content.get(first.len()..first.len() + yaml_len)?;
            let body = content
                .get(consumed..)
                .unwrap_or_default()
                .trim()
                .to_string();
            return Some((yaml, body));
        }
        yaml_len += chunk.len();
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{}-{}", prefix, uuid::Uuid::new_v4()))
    }

    fn write_skill(root: &Path, dir_name: &str, skill_md: &str) {
        let skill_dir = root.join(dir_name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), skill_md).unwrap();
    }

    #[test]
    fn discovers_workspace_skills() {
        let workspace = temp_dir("lightclaw-skills");
        let workspace_skills = workspace.join("skills");
        write_skill(
            &workspace_skills,
            "weather",
            r#"---
name: weather
description: Check weather
---
Use this skill to check weather.
"#,
        );

        let manager = SkillManager {
            roots: vec![SkillRoot {
                path: workspace_skills.clone(),
                source: "workspace".to_string(),
            }],
        };
        let skills = manager.discover_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "weather");

        let _ = std::fs::remove_dir_all(workspace);
    }

    #[test]
    fn precedence_uses_later_roots() {
        let root = temp_dir("lightclaw-skills-precedence");
        let personal = root.join("personal");
        let workspace = root.join("workspace");
        write_skill(
            &personal,
            "demo",
            r#"---
name: demo
description: Personal version
---
Personal body
"#,
        );
        write_skill(
            &workspace,
            "demo",
            r#"---
name: demo
description: Workspace version
---
Workspace body
"#,
        );

        let manager = SkillManager {
            roots: vec![
                SkillRoot {
                    path: personal,
                    source: "agents-personal".to_string(),
                },
                SkillRoot {
                    path: workspace,
                    source: "workspace".to_string(),
                },
            ],
        };

        let (meta, body) = manager.load_skill_checked("demo").unwrap();
        assert_eq!(meta.description, "Workspace version");
        assert!(body.contains("Workspace body"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_missing_dependencies() {
        let root = temp_dir("lightclaw-skills-deps");
        write_skill(
            &root,
            "needs-tool",
            r#"---
name: needs-tool
description: Needs deps
deps:
  - totally-not-a-real-binary-12345
---
Requires a dependency.
"#,
        );
        let manager = SkillManager {
            roots: vec![SkillRoot {
                path: root.clone(),
                source: "workspace".to_string(),
            }],
        };

        let err = manager.load_skill_checked("needs-tool").unwrap_err();
        assert!(err.contains("missing required dependencies"));

        let _ = std::fs::remove_dir_all(root);
    }
}
