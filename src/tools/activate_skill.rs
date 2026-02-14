use crate::skills::SkillManager;
use crate::tools::ToolError;
use rig::completion::request::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;

#[derive(Clone)]
pub struct ActivateSkillTool {
    skill_manager: SkillManager,
}

impl ActivateSkillTool {
    pub fn new(skill_manager: SkillManager) -> Self {
        Self { skill_manager }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ActivateSkillArgs {
    /// The skill name to activate
    pub skill_name: String,
}

impl Tool for ActivateSkillTool {
    const NAME: &'static str = "activate_skill";
    type Args = ActivateSkillArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description:
                    "Activate a skill by name and load its full instructions from SKILL.md."
                        .to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(ActivateSkillArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        let manager = self.skill_manager.clone();
        async move {
            let skill_name = args.skill_name.trim();
            if skill_name.is_empty() {
                return Err(ToolError::msg("Missing required field: skill_name"));
            }

            match manager.load_skill_checked(skill_name) {
                Ok((meta, body)) => {
                    let mut out = format!("# Skill: {}\n\n", meta.name);
                    out.push_str(&format!("Description: {}\n", meta.description));
                    out.push_str(&format!("Skill directory: {}\n", meta.dir_path.display()));
                    out.push_str(&format!("Source: {}\n", meta.source));
                    if let Some(version) = meta.version {
                        out.push_str(&format!("Version: {}\n", version));
                    }
                    if let Some(updated_at) = meta.updated_at {
                        out.push_str(&format!("Updated at: {}\n", updated_at));
                    }
                    if !meta.platforms.is_empty() {
                        out.push_str(&format!("Platforms: {}\n", meta.platforms.join(", ")));
                    }
                    if !meta.deps.is_empty() {
                        out.push_str(&format!("Dependencies: {}\n", meta.deps.join(", ")));
                    }
                    out.push_str("\n## Instructions\n\n");
                    out.push_str(&body);
                    Ok(out)
                }
                Err(err) => Ok(format!("Error: {err}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::path::PathBuf;

    fn temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{}-{}", prefix, uuid::Uuid::new_v4()))
    }

    fn write_skill(root: &Path, name: &str, description: &str, instructions: &str) {
        let skill_dir = root.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        let content =
            format!("---\nname: {name}\ndescription: {description}\n---\n{instructions}\n");
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();
    }

    #[tokio::test]
    async fn activate_skill_returns_instructions() {
        let workspace = temp_dir("femtobot-activate-skill");
        let skills_dir = workspace.join("skills");
        write_skill(
            &skills_dir,
            "demo",
            "Demo skill",
            "Use this skill for demo tasks.",
        );

        let tool = ActivateSkillTool::new(SkillManager::from_workspace_dir(workspace.as_path()));
        let out = tool
            .call(ActivateSkillArgs {
                skill_name: "demo".to_string(),
            })
            .await
            .unwrap();

        assert!(out.contains("# Skill: demo"));
        assert!(out.contains("Demo skill"));
        assert!(out.contains("Use this skill for demo tasks."));

        let _ = std::fs::remove_dir_all(workspace);
    }
}
