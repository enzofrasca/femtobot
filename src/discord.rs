use crate::bus::{InboundMessage, MessageBus, OutboundMessage};
use crate::config::AppConfig;
use anyhow::{anyhow, Result};
use serenity::async_trait;
use serenity::http::Http;
use serenity::model::channel::Message as DiscordMessage;
use serenity::model::gateway::Ready;
use serenity::model::id::ChannelId;
use serenity::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{info, warn};

const DISCORD_MESSAGE_LIMIT: usize = 2000;

pub async fn start(cfg: AppConfig, bus: MessageBus) -> Result<()> {
    let token = cfg.channels.discord.bot_token.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("discord token is missing"));
    }

    let intents = GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::DIRECT_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT;

    let handler = DiscordHandler::new(&cfg, bus.clone());
    let mut client = Client::builder(token, intents)
        .event_handler(handler)
        .await
        .map_err(|err| anyhow!("discord client initialization failed: {err}"))?;

    spawn_outbound_forwarder(client.http.clone(), bus.subscribe_outbound());

    client
        .start()
        .await
        .map_err(|err| anyhow!("discord runtime error: {err}"))?;
    Ok(())
}

struct DiscordHandler {
    bus: MessageBus,
    allowed_channels: HashSet<u64>,
    allow_from: Vec<String>,
}

impl DiscordHandler {
    fn new(cfg: &AppConfig, bus: MessageBus) -> Self {
        let allowed_channels = cfg
            .channels
            .discord
            .allowed_channels
            .iter()
            .filter_map(|raw| raw.trim().parse::<u64>().ok())
            .collect::<HashSet<_>>();
        let allow_from = cfg
            .channels
            .discord
            .allow_from
            .iter()
            .map(|entry| entry.trim().to_ascii_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect::<Vec<_>>();
        Self {
            bus,
            allowed_channels,
            allow_from,
        }
    }

    fn is_channel_allowed(&self, msg: &DiscordMessage) -> bool {
        if self.allowed_channels.is_empty() || msg.guild_id.is_none() {
            return true;
        }
        self.allowed_channels.contains(&msg.channel_id.get())
    }

    fn is_sender_allowed(&self, msg: &DiscordMessage) -> bool {
        if self.allow_from.is_empty() {
            return true;
        }
        let uid = msg.author.id.get().to_string();
        let uname = msg.author.name.to_ascii_lowercase();
        let mention = format!("<@{}>", msg.author.id.get());
        self.allow_from.iter().any(|allowed| {
            allowed == &uid
                || allowed == &uname
                || allowed == &format!("@{uname}")
                || allowed == &mention
        })
    }
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn message(&self, ctx: Context, msg: DiscordMessage) {
        if msg.author.bot {
            return;
        }
        if !self.is_channel_allowed(&msg) || !self.is_sender_allowed(&msg) {
            return;
        }

        let text = msg.content.trim().to_string();
        if text.is_empty() {
            return;
        }

        if msg.guild_id.is_some() {
            let bot_id = ctx.cache.current_user().id;
            let mentioned = msg.mentions.iter().any(|user| user.id == bot_id);
            if !mentioned {
                return;
            }
        }

        let _typing = msg.channel_id.start_typing(&ctx.http);

        self.bus
            .publish_inbound(InboundMessage {
                channel: "discord".to_string(),
                chat_id: msg.channel_id.get().to_string(),
                sender_id: msg.author.id.get().to_string(),
                content: text,
            })
            .await;
    }

    async fn ready(&self, _ctx: Context, ready: Ready) {
        info!("discord connected as {}", ready.user.name);
    }
}

fn spawn_outbound_forwarder(
    http: Arc<Http>,
    mut rx: tokio::sync::broadcast::Receiver<OutboundMessage>,
) {
    tokio::spawn(async move {
        loop {
            let msg = match rx.recv().await {
                Ok(msg) => msg,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("outbound channel closed, discord forwarder shutting down");
                    break;
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!("discord outbound lagged, skipped {skipped} message(s)");
                    continue;
                }
            };

            if msg.channel != "discord" {
                continue;
            }

            let Ok(raw_channel_id) = msg.chat_id.parse::<u64>() else {
                warn!("invalid discord chat_id: {}", msg.chat_id);
                continue;
            };

            if let Err(err) =
                send_discord_message(&http, ChannelId::new(raw_channel_id), &msg.content).await
            {
                warn!("discord send failed for channel {}: {err}", msg.chat_id);
            }
        }
    });
}

async fn send_discord_message(
    http: &Http,
    channel_id: ChannelId,
    text: &str,
) -> serenity::Result<()> {
    if text.len() <= DISCORD_MESSAGE_LIMIT {
        channel_id.say(http, text).await?;
        return Ok(());
    }

    let mut remaining = text;
    while !remaining.is_empty() {
        let chunk_len = if remaining.len() <= DISCORD_MESSAGE_LIMIT {
            remaining.len()
        } else {
            remaining[..DISCORD_MESSAGE_LIMIT]
                .rfind('\n')
                .unwrap_or(DISCORD_MESSAGE_LIMIT)
        };
        let chunk = &remaining[..chunk_len];
        channel_id.say(http, chunk).await?;
        remaining = &remaining[chunk_len..];
        if remaining.starts_with('\n') {
            remaining = &remaining[1..];
        }
    }
    Ok(())
}
