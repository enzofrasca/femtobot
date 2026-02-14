use crate::tools::fs;
use crate::tools::ToolError;
use regex::Regex;
use rig::completion::request::ToolDefinition;
use rig::tool::Tool;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::process::Command;

#[derive(Clone)]
pub struct ShellGuard {
    deny: Vec<Regex>,
    allow: Vec<Regex>,
}

impl ShellGuard {
    pub fn new() -> Self {
        let deny = vec![
            // rm with short and long flags
            Regex::new(r"\brm\s+-[rf]{1,2}\b").unwrap(),
            Regex::new(r"\brm\s+--recursive\b").unwrap(),
            Regex::new(r"\brm\s+--force\b").unwrap(),
            // Windows destructive commands
            Regex::new(r"\bdel\s+/[fq]\b").unwrap(),
            Regex::new(r"\brmdir\s+/s\b").unwrap(),
            // Disk formatting / partitioning
            Regex::new(r"\b(format|mkfs|diskpart)\b").unwrap(),
            // dd - read from or write to block devices
            Regex::new(r"\bdd\s+if=").unwrap(),
            Regex::new(r"\bdd\b.*\bof=/dev/").unwrap(),
            // Redirect to block devices
            Regex::new(r">\s*/dev/(sd|hd|nvme|vd)").unwrap(),
            // System power commands
            Regex::new(r"\b(shutdown|reboot|poweroff)\b").unwrap(),
            // find with destructive actions
            Regex::new(r"\bfind\b.*\s-delete\b").unwrap(),
            Regex::new(r"\bfind\b.*-exec\s+rm\b").unwrap(),
            // Piping untrusted downloads to shell
            Regex::new(r"\b(curl|wget)\b[^|]*\|\s*(sudo\s+)?(sh|bash|zsh)\b").unwrap(),
            // chmod 777 on system paths
            Regex::new(r"\bchmod\s+(-[a-zA-Z]*\s+)*777\s+/").unwrap(),
            // Fork bomb pattern
            Regex::new(r":\(\)\s*\{").unwrap(),
        ];
        Self {
            deny,
            allow: vec![],
        }
    }

    pub fn check(&self, cmd: &str) -> Result<(), String> {
        let lower = cmd.to_lowercase();
        for re in &self.deny {
            if re.is_match(&lower) {
                return Err("blocked by safety guard (dangerous pattern detected)".to_string());
            }
        }
        if !self.allow.is_empty() {
            if !self.allow.iter().any(|r| r.is_match(&lower)) {
                return Err("blocked by safety guard (not in allowlist)".to_string());
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ExecTool {
    guard: ShellGuard,
    timeout_secs: u64,
    working_dir: PathBuf,
    /// When set, working_dir arg must resolve to a path under this directory.
    allowed_dir: Option<PathBuf>,
}

impl ExecTool {
    pub fn new(timeout_secs: u64, working_dir: PathBuf, allowed_dir: Option<PathBuf>) -> Self {
        Self {
            guard: ShellGuard::new(),
            timeout_secs,
            working_dir,
            allowed_dir,
        }
    }
}

#[cfg(target_os = "windows")]
fn build_shell_command(command: &str, cwd: &Path) -> Result<(Command, Option<Command>), ToolError> {
    let mut primary = if let Some(comspec) = std::env::var_os("ComSpec") {
        Command::new(comspec)
    } else {
        Command::new("cmd.exe")
    };
    primary.arg("/C").arg(command).current_dir(cwd);
    primary.stdout(std::process::Stdio::piped());
    primary.stderr(std::process::Stdio::piped());

    let mut fallback = Command::new("cmd.exe");
    fallback.arg("/C").arg(command).current_dir(cwd);
    fallback.stdout(std::process::Stdio::piped());
    fallback.stderr(std::process::Stdio::piped());

    Ok((primary, Some(fallback)))
}

#[cfg(not(target_os = "windows"))]
fn build_shell_command(command: &str, cwd: &Path) -> Result<(Command, Option<Command>), ToolError> {
    let shell = if Path::new("/bin/sh").exists() {
        "/bin/sh"
    } else {
        "sh"
    };

    let mut primary = Command::new(shell);
    primary.arg("-c").arg(command).current_dir(cwd);
    primary.stdout(std::process::Stdio::piped());
    primary.stderr(std::process::Stdio::piped());

    let fallback = if shell == "/bin/sh" { "sh" } else { "/bin/sh" };
    let mut fallback_cmd = Command::new(fallback);
    fallback_cmd.arg("-c").arg(command).current_dir(cwd);
    fallback_cmd.stdout(std::process::Stdio::piped());
    fallback_cmd.stderr(std::process::Stdio::piped());

    Ok((primary, Some(fallback_cmd)))
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct ExecArgs {
    /// The shell command to execute
    pub command: String,
    /// Optional working directory for the command
    pub working_dir: Option<String>,
}

impl Tool for ExecTool {
    const NAME: &'static str = "exec";
    type Args = ExecArgs;
    type Output = String;
    type Error = ToolError;

    fn definition(
        &self,
        _prompt: String,
    ) -> impl std::future::Future<Output = ToolDefinition> + Send {
        async {
            ToolDefinition {
                name: Self::NAME.to_string(),
                description: "Execute a shell command and return its output. Use with caution."
                    .to_string(),
                parameters: serde_json::to_value(schemars::schema_for!(ExecArgs)).unwrap(),
            }
        }
    }

    fn call(
        &self,
        args: Self::Args,
    ) -> impl std::future::Future<Output = Result<Self::Output, Self::Error>> + Send {
        async move {
            self.guard.check(&args.command).map_err(ToolError::msg)?;

            let cwd = match args.working_dir.as_deref() {
                Some(s) => fs::resolve_path(s, self.allowed_dir.as_deref(), true)
                    .map_err(ToolError::msg)?,
                None => self.working_dir.clone(),
            };

            let (mut cmd, fallback) = build_shell_command(&args.command, &cwd)?;

            let mut child = match cmd.spawn() {
                Ok(child) => child,
                Err(err) => {
                    if let Some(mut retry) = fallback {
                        retry.spawn().map_err(|e| {
                            ToolError::msg(format!(
                                "failed to launch shell command: {err}; fallback also failed: {e}"
                            ))
                        })?
                    } else {
                        return Err(ToolError::msg(format!(
                            "failed to launch shell command: {err}"
                        )));
                    }
                }
            };
            let timeout = tokio::time::Duration::from_secs(self.timeout_secs);

            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();

            let read_stdout = async move {
                let mut buf = Vec::new();
                if let Some(mut s) = stdout.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_end(&mut buf).await;
                }
                buf
            };
            let read_stderr = async move {
                let mut buf = Vec::new();
                if let Some(mut s) = stderr.take() {
                    use tokio::io::AsyncReadExt;
                    let _ = s.read_to_end(&mut buf).await;
                }
                buf
            };

            let output_status = tokio::select! {
                status = child.wait() => status.map_err(|e| ToolError::msg(e.to_string()))?,
                _ = tokio::time::sleep(timeout) => {
                    let _ = child.kill().await;
                    return Ok(format!(
                        "Error: Command timed out after {} seconds",
                        self.timeout_secs
                    ));
                }
            };

            let (out_buf, err_buf) = tokio::join!(read_stdout, read_stderr);

            let mut parts = Vec::new();
            if !out_buf.is_empty() {
                parts.push(String::from_utf8_lossy(&out_buf).to_string());
            }
            if !err_buf.is_empty() {
                let stderr_text = String::from_utf8_lossy(&err_buf).to_string();
                if !stderr_text.trim().is_empty() {
                    parts.push(format!("STDERR:\n{stderr_text}"));
                }
            }
            if !output_status.success() {
                parts.push(format!(
                    "\nExit code: {}",
                    output_status.code().unwrap_or(-1)
                ));
            }

            let mut result = if parts.is_empty() {
                "(no output)".to_string()
            } else {
                parts.join("\n")
            };

            let max_len = 10000;
            if result.len() > max_len {
                let extra = result.len() - max_len;
                result.truncate(max_len);
                result.push_str(&format!("\n... (truncated, {extra} more chars)"));
            }

            Ok(result)
        }
    }
}
