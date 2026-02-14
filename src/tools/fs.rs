use crate::tools::ToolError;
use rig::completion::request::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use std::path::{Path, PathBuf};

fn expand_path(raw: &str) -> PathBuf {
    if raw == "~" || raw.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            let trimmed = raw.trim_start_matches('~');
            return home.join(trimmed.trim_start_matches('/'));
        }
    }
    PathBuf::from(raw)
}

/// Resolve a path and optionally enforce that it is under `allowed_dir`.
/// Used by file tools and by exec (for working_dir) when restrict_to_workspace is true.
pub(crate) fn resolve_path(
    path: &str,
    allowed_dir: Option<&Path>,
    allow_missing: bool,
) -> Result<PathBuf, String> {
    let expanded = expand_path(path);
    let abs = if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(expanded)
    };

    let resolved = if allow_missing {
        if abs.exists() {
            abs.canonicalize().map_err(|e| e.to_string())?
        } else {
            abs
        }
    } else {
        abs.canonicalize().map_err(|e| e.to_string())?
    };

    if let Some(allowed) = allowed_dir {
        let allowed = allowed
            .canonicalize()
            .map_err(|e| format!("failed to resolve allowed dir: {e}"))?;
        if !resolved.starts_with(&allowed) {
            return Err(format!(
                "path {} is outside allowed directory {}",
                resolved.display(),
                allowed.display()
            ));
        }
    }

    Ok(resolved)
}

#[derive(Clone)]
pub struct ReadFileTool {
    allowed_dir: Option<PathBuf>,
}

impl ReadFileTool {
    pub fn new(allowed_dir: Option<PathBuf>) -> Self {
        Self { allowed_dir }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ReadFileArgs {
    /// The file path to read
    pub path: String,
}

impl Tool for ReadFileTool {
    const NAME: &'static str = "read_file";
    type Args = ReadFileArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Read the contents of a file at the given path.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(ReadFileArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            let path = resolve_path(&args.path, self.allowed_dir.as_deref(), false)
                .map_err(ToolError::msg)?;
            if !path.exists() {
                return Ok(format!("Error: File not found: {}", args.path));
            }
            if !path.is_file() {
                return Ok(format!("Error: Not a file: {}", args.path));
            }
            match tokio::fs::read_to_string(&path).await {
                Ok(content) => Ok(content),
                Err(e) => Ok(format!("Error reading file: {e}")),
            }
        }
    }
}

#[derive(Clone)]
pub struct WriteFileTool {
    allowed_dir: Option<PathBuf>,
}

impl WriteFileTool {
    pub fn new(allowed_dir: Option<PathBuf>) -> Self {
        Self { allowed_dir }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct WriteFileArgs {
    /// The file path to write to
    pub path: String,
    /// The content to write
    pub content: String,
}

impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";
    type Args = WriteFileArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Write content to a file at the given path. Creates parent directories if needed.".to_string(),
            parameters: serde_json::to_value(schemars::schema_for!(WriteFileArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            let path = resolve_path(&args.path, self.allowed_dir.as_deref(), true)
                .map_err(ToolError::msg)?;
            if let Some(parent) = path.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    return Ok(format!("Error creating parent directories: {e}"));
                }
            }
            match tokio::fs::write(&path, args.content.as_bytes()).await {
                Ok(_) => Ok(format!(
                    "Successfully wrote {} bytes to {}",
                    args.content.len(),
                    args.path
                )),
                Err(e) => Ok(format!("Error writing file: {e}")),
            }
        }
    }
}

#[derive(Clone)]
pub struct EditFileTool {
    allowed_dir: Option<PathBuf>,
}

impl EditFileTool {
    pub fn new(allowed_dir: Option<PathBuf>) -> Self {
        Self { allowed_dir }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct EditFileArgs {
    /// The file path to edit
    pub path: String,
    /// The exact text to find and replace
    pub old_text: String,
    /// The text to replace with
    pub new_text: String,
}

impl Tool for EditFileTool {
    const NAME: &'static str = "edit_file";
    type Args = EditFileArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Edit a file by replacing old_text with new_text. The old_text must exist exactly in the file.".to_string(),
            parameters: serde_json::to_value(schemars::schema_for!(EditFileArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            let path = resolve_path(&args.path, self.allowed_dir.as_deref(), false)
                .map_err(ToolError::msg)?;
            if !path.exists() {
                return Ok(format!("Error: File not found: {}", args.path));
            }
            let content = match tokio::fs::read_to_string(&path).await {
                Ok(c) => c,
                Err(e) => return Ok(format!("Error reading file: {e}")),
            };
            if !content.contains(&args.old_text) {
                return Ok(
                    "Error: old_text not found in file. Make sure it matches exactly.".to_string(),
                );
            }
            let count = content.matches(&args.old_text).count();
            if count > 1 {
                return Ok(format!(
                "Warning: old_text appears {count} times. Please provide more context to make it unique."
            ));
            }
            let new_content = content.replacen(&args.old_text, &args.new_text, 1);
            match tokio::fs::write(&path, new_content.as_bytes()).await {
                Ok(_) => Ok(format!("Successfully edited {}", args.path)),
                Err(e) => Ok(format!("Error editing file: {e}")),
            }
        }
    }
}

#[derive(Clone)]
pub struct ListDirTool {
    allowed_dir: Option<PathBuf>,
}

impl ListDirTool {
    pub fn new(allowed_dir: Option<PathBuf>) -> Self {
        Self { allowed_dir }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ListDirArgs {
    /// The directory path to list
    pub path: String,
}

impl Tool for ListDirTool {
    const NAME: &'static str = "list_dir";
    type Args = ListDirArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "List the contents of a directory.".to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(ListDirArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            let path = resolve_path(&args.path, self.allowed_dir.as_deref(), false)
                .map_err(ToolError::msg)?;
            if !path.exists() {
                return Ok(format!("Error: Directory not found: {}", args.path));
            }
            if !path.is_dir() {
                return Ok(format!("Error: Not a directory: {}", args.path));
            }
            let mut items = Vec::new();
            let mut entries_raw = match tokio::fs::read_dir(&path).await {
                Ok(iter) => iter,
                Err(e) => return Ok(format!("Error listing directory: {e}")),
            };
            let mut entries = Vec::new();
            while let Ok(Some(entry)) = entries_raw.next_entry().await {
                entries.push(entry);
            }
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let p = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();
                let prefix = if p.is_dir() { "DIR " } else { "FILE " };
                items.push(format!("{prefix}{name}"));
            }
            if items.is_empty() {
                return Ok(format!("Directory {} is empty", args.path));
            }
            Ok(items.join("\n"))
        }
    }
}
