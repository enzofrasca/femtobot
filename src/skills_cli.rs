use crate::config::AppConfig;
use crate::skillhub::{
    ClawhubInstallRequest, ClawhubSearchResult, InstalledSkill, Skillhub, SkillsShInstallRequest,
    SkillsShSearchResult, SkillsSourceInstallRequest, SourceSkill,
};
use anyhow::{anyhow, Result};
use clap::{Subcommand, ValueEnum};
use std::path::Path;

#[derive(Subcommand, Debug)]
pub enum SkillsCommands {
    /// Search skills on ClawHub
    Search {
        /// Plain-language query
        query: String,
        /// Limit number of results
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Search skills via skills.sh
    Find {
        /// Keyword query
        query: Option<String>,
        /// Limit number of results
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Install a skill via ClawHub or skills.sh/source
    Install {
        /// Installation backend
        #[arg(long, value_enum, default_value_t = SkillInstaller::Clawhub)]
        from: SkillInstaller,
        /// ClawHub slug, skills.sh slug/query, or source path/repo/url
        target: String,
        /// ClawHub: install a specific version
        #[arg(long)]
        version: Option<String>,
        /// Overwrite existing folder
        #[arg(long, default_value_t = false)]
        force: bool,
        /// skills source: install specific skill name(s)
        #[arg(long = "skill")]
        skills: Vec<String>,
        /// List available items without installing
        #[arg(long, default_value_t = false)]
        list: bool,
        /// Compatibility no-op (native installer is non-interactive)
        #[arg(long, default_value_t = false)]
        yes: bool,
        /// skills source: install all detected skills
        #[arg(long, default_value_t = false)]
        all: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum SkillInstaller {
    Clawhub,
    Skills,
}

pub fn handle_skills(command: SkillsCommands) -> Result<()> {
    let cfg = AppConfig::load_relaxed();
    std::fs::create_dir_all(&cfg.workspace_dir)?;

    let skills_root = cfg.workspace_dir.join("skills");
    std::fs::create_dir_all(&skills_root)?;

    let hub = Skillhub::new()?;

    match command {
        SkillsCommands::Search { query, limit } => {
            let results = hub.search_clawhub(&query, normalize_limit(limit, 10))?;
            print_clawhub_results(&query, &results);
            Ok(())
        }
        SkillsCommands::Find { query, limit } => {
            let query = query
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow!("`femtobot skills find` requires a query"))?;
            let results = hub.search_skills_sh(query, normalize_limit(limit, 10))?;
            print_skills_sh_results(query, &results);
            Ok(())
        }
        SkillsCommands::Install {
            from,
            target,
            version,
            force,
            skills,
            list,
            yes,
            all,
        } => {
            if yes {
                eprintln!(
                    "--yes is a compatibility flag and has no effect with the native installer"
                );
            }
            match from {
                SkillInstaller::Clawhub => {
                    if !skills.is_empty() || list || all {
                        return Err(anyhow!(
                            "--skill/--list/--all are only supported with --from skills"
                        ));
                    }

                    let installed = hub.install_from_clawhub(ClawhubInstallRequest {
                        slug: target,
                        version,
                        tag: None,
                        skills_root,
                        force,
                    })?;
                    print_installed_skills(&[installed]);
                    Ok(())
                }
                SkillInstaller::Skills => {
                    if version.is_some() {
                        return Err(anyhow!("--version is only supported with --from clawhub"));
                    }
                    if all && !skills.is_empty() {
                        return Err(anyhow!("use either --all or one/more --skill filters"));
                    }

                    if list {
                        if looks_like_source(&target) {
                            let discovered = hub.list_from_skills_source(&target)?;
                            print_source_listing(&target, &discovered);
                        } else {
                            let results = hub.search_skills_sh(&target, 25)?;
                            print_skills_sh_results(&target, &results);
                        }
                        return Ok(());
                    }

                    let installed = if looks_like_source(&target) {
                        let filters = if all { Vec::new() } else { skills };
                        hub.install_from_skills_source(SkillsSourceInstallRequest {
                            source: target,
                            skill_filters: filters,
                            skills_root,
                            force,
                        })?
                    } else if all || !skills.is_empty() {
                        let selected = resolve_skills_sh_target(&hub, &target)?;
                        let source = if selected.source.trim().is_empty() {
                            selected.slug.clone()
                        } else {
                            selected.source.clone()
                        };

                        let filters = if all {
                            Vec::new()
                        } else if !skills.is_empty() {
                            skills
                        } else {
                            vec![selected.name]
                        };

                        hub.install_from_skills_source(SkillsSourceInstallRequest {
                            source,
                            skill_filters: filters,
                            skills_root,
                            force,
                        })?
                    } else {
                        hub.install_from_skills_sh(SkillsShInstallRequest {
                            slug_or_query: target,
                            skills_root,
                            force,
                        })?
                    };

                    print_installed_skills(&installed);
                    Ok(())
                }
            }
        }
    }
}

fn normalize_limit(limit: Option<u32>, default_value: usize) -> usize {
    let raw = limit.unwrap_or(default_value as u32).max(1);
    raw.min(100) as usize
}

fn resolve_skills_sh_target(hub: &Skillhub, query: &str) -> Result<SkillsShSearchResult> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("target cannot be empty"));
    }

    let results = hub.search_skills_sh(trimmed, 25)?;
    if results.is_empty() {
        return Err(anyhow!("no skills.sh results found for query: {trimmed}"));
    }

    Ok(results
        .iter()
        .find(|entry| entry.slug.eq_ignore_ascii_case(trimmed))
        .or_else(|| {
            results
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(trimmed))
        })
        .unwrap_or(&results[0])
        .clone())
}

fn looks_like_source(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    Path::new(trimmed).is_absolute()
        || matches!(trimmed, "." | "..")
        || trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("git@")
        || trimmed.ends_with(".git")
        || trimmed.contains('/')
}

fn print_clawhub_results(query: &str, results: &[ClawhubSearchResult]) {
    if results.is_empty() {
        println!("No ClawHub results for '{query}'.");
        return;
    }

    println!("ClawHub results for '{query}':");
    for (index, item) in results.iter().enumerate() {
        let title = item.display_name.as_deref().unwrap_or(&item.slug);
        let version = item.version.as_deref().unwrap_or("-");
        let summary = truncate(&item.summary.clone().unwrap_or_default(), 110);
        println!(
            "{:>2}. {} ({}) [v{}] {}",
            index + 1,
            title,
            item.slug,
            version,
            summary
        );
    }
}

fn print_skills_sh_results(query: &str, results: &[SkillsShSearchResult]) {
    if results.is_empty() {
        println!("No skills.sh results for '{query}'.");
        return;
    }

    println!("skills.sh results for '{query}':");
    for (index, item) in results.iter().enumerate() {
        let source = truncate(&item.source, 70);
        println!(
            "{:>2}. {} ({}) installs={} source={}",
            index + 1,
            item.name,
            item.slug,
            item.installs,
            source
        );
    }
}

fn print_source_listing(source: &str, discovered: &[SourceSkill]) {
    println!("Detected skills in '{source}':");
    for (index, skill) in discovered.iter().enumerate() {
        let display_name = skill
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("(unnamed)");
        println!("{:>2}. {} [{}]", index + 1, display_name, skill.directory);
    }
}

fn print_installed_skills(installed: &[InstalledSkill]) {
    if installed.is_empty() {
        println!("No skills installed.");
        return;
    }

    println!("Installed {} skill(s):", installed.len());
    for skill in installed {
        match skill.version.as_deref() {
            Some(version) => println!(
                "- {} -> {} (source: {}, version: {})",
                skill.install_name,
                skill.path.display(),
                skill.source,
                version
            ),
            None => println!(
                "- {} -> {} (source: {})",
                skill.install_name,
                skill.path.display(),
                skill.source
            ),
        }
    }
}

fn truncate(value: &str, max_len: usize) -> String {
    let single_line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= max_len {
        return single_line;
    }

    let mut out = String::new();
    for ch in single_line.chars().take(max_len.saturating_sub(1)) {
        out.push(ch);
    }
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_detection_handles_common_inputs() {
        assert!(looks_like_source("vercel-labs/agent-skills"));
        assert!(looks_like_source("./skills"));
        assert!(looks_like_source(
            "https://github.com/vercel-labs/agent-skills"
        ));
        assert!(!looks_like_source("weather"));
    }

    #[test]
    fn limit_is_clamped() {
        assert_eq!(normalize_limit(Some(0), 10), 1);
        assert_eq!(normalize_limit(Some(5), 10), 5);
        assert_eq!(normalize_limit(Some(300), 10), 100);
        assert_eq!(normalize_limit(None, 12), 12);
    }
}
