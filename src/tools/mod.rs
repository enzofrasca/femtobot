use crate::bus::MessageBus;
use crate::config::{AppConfig, MemoryMode};
use crate::cron::CronService;
use crate::memory::simple::file_store::MemoryStore;
use crate::memory::smart::vector_store::VectorMemoryStore;
use crate::skills::SkillManager;

pub mod activate_skill;
pub mod cron;
pub mod fs;
pub mod memory;
pub mod send;
pub mod shell;
pub mod web;

#[derive(Debug)]
pub struct ToolError(String);

impl ToolError {
    pub fn msg(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ToolError {}

#[derive(Clone)]
pub struct ToolRegistry {
    pub read_file: fs::ReadFileTool,
    pub write_file: fs::WriteFileTool,
    pub edit_file: fs::EditFileTool,
    pub list_dir: fs::ListDirTool,
    pub exec: shell::ExecTool,
    pub web_search: web::WebSearchTool,
    pub web_fetch: web::WebFetchTool,
    pub activate_skill: activate_skill::ActivateSkillTool,
    pub cron: cron::CronTool,
    pub send_message: send::SendMessageTool,
    pub memory_search: memory::MemorySearchTool,
    pub memory_get: memory::MemoryGetTool,
    pub remember: Option<memory::RememberTool>,
}

impl ToolRegistry {
    pub fn new(
        cfg: AppConfig,
        cron_service: CronService,
        bus: MessageBus,
        memory_store: MemoryStore,
        vector_store: Option<VectorMemoryStore>,
    ) -> Self {
        let allowed_dir = if cfg.tools.restrict_to_workspace {
            Some(cfg.workspace_dir.clone())
        } else {
            None
        };
        let memory_search =
            memory::MemorySearchTool::new(memory_store.clone(), vector_store.clone());
        let memory_get = memory::MemoryGetTool::new(memory_store.clone(), vector_store.clone());
        let remember = match cfg.memory.mode {
            MemoryMode::None => None,
            MemoryMode::Simple => Some(memory::RememberTool::new_file(memory_store.clone())),
            MemoryMode::Smart => vector_store
                .map(|store| memory::RememberTool::new_hybrid(store, memory_store.clone()))
                .or_else(|| Some(memory::RememberTool::new_file(memory_store.clone()))),
        };
        let skill_manager = SkillManager::from_workspace_dir(cfg.workspace_dir.as_path());
        Self {
            read_file: fs::ReadFileTool::new(allowed_dir.clone()),
            write_file: fs::WriteFileTool::new(allowed_dir.clone()),
            edit_file: fs::EditFileTool::new(allowed_dir.clone()),
            list_dir: fs::ListDirTool::new(allowed_dir.clone()),
            exec: shell::ExecTool::new(
                cfg.tools.exec_timeout_secs,
                cfg.workspace_dir.clone(),
                allowed_dir,
            ),
            web_search: web::WebSearchTool::new(
                cfg.tools.web_search_provider.clone(),
                cfg.tools.brave_api_key.clone(),
                cfg.tools.firecrawl_api_key.clone(),
            ),
            web_fetch: web::WebFetchTool::new(
                cfg.tools.web_fetch_provider.clone(),
                cfg.tools.firecrawl_api_key.clone(),
            ),
            activate_skill: activate_skill::ActivateSkillTool::new(skill_manager),
            cron: cron::CronTool::new(cron_service),
            send_message: send::SendMessageTool::new(bus),
            memory_search,
            memory_get,
            remember,
        }
    }
}
