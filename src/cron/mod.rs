pub mod store;
pub mod types;

use crate::bus::{InboundMessage, MessageBus};
use crate::config::AppConfig;
use anyhow::Result;
use chrono::{DateTime, Utc};
use cron::Schedule;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};
use tokio::time::{self, Duration};
use tracing::{error, info};
use types::{CronJob, CronSchedule};

struct CronInner {
    store: Mutex<store::CronStore>,
    bus: MessageBus,
    notify: Notify,
}

#[derive(Clone)]
pub struct CronService {
    inner: Arc<CronInner>,
}

pub struct CronStatus {
    pub jobs: usize,
    pub enabled_jobs: usize,
    pub next_wake_at_ms: Option<i64>,
}

impl CronService {
    pub fn new(cfg: &AppConfig, bus: MessageBus) -> Self {
        let store = store::CronStore::new(cfg.data_dir.clone());
        Self {
            inner: Arc::new(CronInner {
                store: Mutex::new(store),
                bus,
                notify: Notify::new(),
            }),
        }
    }

    pub async fn start(&self) {
        // Load initial state
        {
            let mut store = self.inner.store.lock().await;
            if let Err(e) = store.load() {
                error!("Failed to load cron jobs: {}", e);
            }
            // Recompute next runs on startup
            let now = Utc::now().timestamp_millis();
            for job in &mut store.jobs {
                if job.enabled {
                    job.state.next_run_at_ms = compute_next_run(&job.schedule, now);
                }
            }
            if let Err(e) = store.save() {
                error!("Failed to save cron jobs after recompute: {}", e);
            }
            info!("Cron service started with {} jobs", store.jobs.len());
        }

        let loop_service = self.clone();

        tokio::spawn(async move {
            // Poll frequently so tool/CLI mutations are picked up quickly even
            // when they happen in another CronService instance.
            const MAX_SLEEP: Duration = Duration::from_secs(1);
            loop {
                // Reload persisted store so tool/CLI changes from other CronService
                // instances are picked up by the running scheduler.
                {
                    let mut store = loop_service.inner.store.lock().await;
                    if let Err(e) = store.load() {
                        error!("Failed to reload cron jobs: {}", e);
                    }
                }

                // 1. Calculate time to next job
                let (next_wake_ms, has_jobs) = {
                    let store = loop_service.inner.store.lock().await;
                    let next = store
                        .jobs
                        .iter()
                        .filter(|j| j.enabled && j.state.next_run_at_ms.is_some())
                        .map(|j| j.state.next_run_at_ms.unwrap())
                        .min();
                    (next, !store.jobs.is_empty())
                };

                let now = Utc::now().timestamp_millis();

                // Determine sleep duration
                let raw_sleep_duration = if let Some(wake_ms) = next_wake_ms {
                    if wake_ms > now {
                        Duration::from_millis((wake_ms - now) as u64)
                    } else {
                        Duration::ZERO // Run immediately
                    }
                } else {
                    // No scheduled jobs. Wake periodically so externally-added jobs
                    // (tool/CLI) are discovered even without this instance's Notify.
                    MAX_SLEEP
                };
                let sleep_duration = std::cmp::min(raw_sleep_duration, MAX_SLEEP);

                if has_jobs && next_wake_ms.is_some() {
                    // Only log if there's actually something scheduled reasonably soon
                }

                tokio::select! {
                    _ = loop_service.inner.notify.notified() => {
                        // Store changed, loop will restart and recompute next wake
                    }
                    _ = time::sleep(sleep_duration) => {
                         // Time to run jobs?
                         if next_wake_ms.is_some() {
                             loop_service.process_due_jobs().await;
                         }
                    }
                }
            }
        });
    }

    async fn process_due_jobs(&self) {
        let mut store = self.inner.store.lock().await;
        // Reload right before execution to avoid running stale jobs and
        // overwriting newer tool/CLI changes with in-memory state.
        if let Err(e) = store.load() {
            error!("Failed to reload cron jobs before execution: {}", e);
            return;
        }
        let now = Utc::now().timestamp_millis();

        let mut jobs_to_run = Vec::new();

        for (i, job) in store.jobs.iter().enumerate() {
            if job.enabled {
                if let Some(next) = job.state.next_run_at_ms {
                    if now >= next {
                        jobs_to_run.push(i);
                    }
                }
            }
        }

        for idx in jobs_to_run {
            let job = &mut store.jobs[idx];
            info!("Executing cron job: {} ({})", job.name, job.id);

            // Send message to bus
            let msg = InboundMessage {
                channel: job
                    .payload
                    .channel
                    .clone()
                    .unwrap_or_else(|| "cron".to_string()),
                chat_id: job
                    .payload
                    .to
                    .clone()
                    .unwrap_or_else(|| "direct".to_string()),
                sender_id: "cron".to_string(),
                content: job.payload.message.clone(),
                // TODO: Propagate job.payload.model when InboundMessage supports it
                // For now, we just ensure the field exists in CronPayload
            };
            self.inner.bus.publish_inbound(msg).await;

            // Update state
            job.state.last_run_at_ms = Some(now);
            job.state.last_status = Some("ok".to_string());
            job.updated_at_ms = now;

            // Handle one-off vs recurring
            if job.schedule.kind == "at" {
                if job.delete_after_run {
                    job.enabled = false;
                    job.state.next_run_at_ms = None;
                } else {
                    job.enabled = false;
                    job.state.next_run_at_ms = None;
                }
            } else {
                job.state.next_run_at_ms = compute_next_run(&job.schedule, now);
            }
        }

        // Save state
        if let Err(e) = store.save() {
            error!("Failed to save cron store: {}", e);
        }
    }

    // CLI helpers
    pub async fn add_job(
        &self,
        name: String,
        schedule: String,
        message: String,
        channel: Option<String>,
        to: Option<String>,
    ) -> Result<()> {
        let mut store = self.inner.store.lock().await;
        store.load()?;
        let now = Utc::now().timestamp_millis();

        // Determine schedule type
        let (kind, every_ms, expr) = if schedule.starts_with("@") || schedule.contains(" *") {
            ("cron", None, Some(schedule))
        } else if let Ok(secs) = schedule.parse::<u64>() {
            ("every", Some((secs * 1000) as i64), None)
        } else {
            ("every", None, None) // Default/Error fallthrough
        };

        if kind == "every" && every_ms.is_none() {
            return Err(anyhow::anyhow!("Invalid schedule format"));
        }

        let sched = CronSchedule {
            kind: kind.to_string(),
            at_ms: None,
            every_ms,
            expr,
            tz: None,
        };

        let next = compute_next_run(&sched, now);

        let job = CronJob {
            id: uuid::Uuid::new_v4().to_string()[..8].to_string(),
            name,
            enabled: true,
            schedule: sched,
            payload: types::CronPayload {
                kind: "agent_turn".to_string(),
                message,
                deliver: false,
                channel,
                to,
                model: None, // Default
            },
            state: types::CronState {
                next_run_at_ms: next,
                ..Default::default()
            },
            created_at_ms: now,
            updated_at_ms: now,
            delete_after_run: false,
        };

        store.add(job.clone())?;
        info!("Added job: {}", job.id);

        // Notify the loop to pick up the new job immediately
        self.inner.notify.notify_one();

        Ok(())
    }

    pub async fn list_jobs(&self) -> Result<Vec<CronJob>> {
        let mut store = self.inner.store.lock().await;
        store.load()?;
        Ok(store.jobs.clone())
    }

    pub async fn remove_job(&self, id: &str) -> Result<bool> {
        let mut store = self.inner.store.lock().await;
        store.load()?;
        let removed = store.remove(id)?;
        if removed {
            // Notify loop to update schedule (e.g. if we removed the next job)
            self.inner.notify.notify_one();
        }
        Ok(removed)
    }

    pub async fn status(&self) -> Result<CronStatus> {
        let mut store = self.inner.store.lock().await;
        store.load()?;
        let next_wake_at_ms = store
            .jobs
            .iter()
            .filter(|j| j.enabled && j.state.next_run_at_ms.is_some())
            .map(|j| j.state.next_run_at_ms.unwrap_or_default())
            .min();
        Ok(CronStatus {
            jobs: store.jobs.len(),
            enabled_jobs: store.jobs.iter().filter(|j| j.enabled).count(),
            next_wake_at_ms,
        })
    }
}

fn compute_next_run(schedule: &CronSchedule, now_ms: i64) -> Option<i64> {
    match schedule.kind.as_str() {
        "at" => {
            if let Some(at) = schedule.at_ms {
                if at > now_ms {
                    return Some(at);
                }
            }
            None
        }
        "every" => {
            if let Some(every) = schedule.every_ms {
                Some(now_ms + every)
            } else {
                None
            }
        }
        "cron" => {
            if let Some(expr) = &schedule.expr {
                if let Ok(schedule) = Schedule::from_str(expr) {
                    let dt = DateTime::<Utc>::from(
                        std::time::UNIX_EPOCH + std::time::Duration::from_millis(now_ms as u64),
                    );
                    if let Some(next) = schedule.after(&dt).next() {
                        return Some(next.timestamp_millis());
                    }
                }
            }
            None
        }
        _ => None,
    }
}
