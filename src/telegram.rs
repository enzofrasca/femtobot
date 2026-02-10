use crate::bus::{BusHandle, InboundMessage, MessageBus};
use crate::config::AppConfig;
use crate::transcription::Transcriber;
use anyhow::{anyhow, Result};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use teloxide::dispatching::UpdateHandler;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{ChatAction, FileId, ParseMode};
use tracing::{info, warn};

pub async fn start(cfg: AppConfig, bus: MessageBus, bus_handle: BusHandle) -> Result<()> {
    let bot = Bot::new(cfg.telegram_bot_token.clone());
    bot.get_me()
        .await
        .map_err(|err| anyhow!("telegram authentication failed: {err}"))?;

    spawn_outbound_forwarder(bot.clone(), bus_handle);

    let allowlist = cfg.telegram_allow_from.clone();
    let transcriber = Transcriber::from_config(&cfg);
    let handler: UpdateHandler<anyhow::Error> =
        Update::filter_message().endpoint(move |bot: Bot, msg: Message, bus: MessageBus| {
            let allowlist = allowlist.clone();
            let transcriber = transcriber.clone();
            async move {
                if !is_allowed(&msg, &allowlist) {
                    return Ok(());
                }

                let chat_id = msg.chat.id.0.to_string();
                let sender_id = msg
                    .from
                    .as_ref()
                    .map(|u| u.id.0.to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                if let Some(text) = msg.text() {
                    let inbound = InboundMessage {
                        channel: "telegram".to_string(),
                        chat_id,
                        sender_id,
                        content: text.to_string(),
                    };
                    bus.publish_inbound(inbound).await;
                    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;
                    return Ok(());
                }

                let media = if let Some(voice) = msg.voice() {
                    Some((
                        voice.file.id.clone(),
                        format!("voice_{}.ogg", voice.file.unique_id.0),
                        voice.file.size as usize,
                    ))
                } else if let Some(audio) = msg.audio() {
                    let filename = audio
                        .file_name
                        .clone()
                        .unwrap_or_else(|| format!("audio_{}.mp3", audio.file.unique_id.0));
                    Some((audio.file.id.clone(), filename, audio.file.size as usize))
                } else {
                    None
                };

                if let Some((file_id, filename, file_size)) = media {
                    let Some(transcriber) = transcriber.clone() else {
                        bot.send_message(
                            msg.chat.id,
                            "Voice/audio transcription is not configured.",
                        )
                        .await?;
                        return Ok(());
                    };
                    if file_size > transcriber.max_bytes() {
                        bot.send_message(
                            msg.chat.id,
                            format!(
                                "Audio file is too large ({} bytes). Max allowed is {} bytes.",
                                file_size,
                                transcriber.max_bytes()
                            ),
                        )
                        .await?;
                        return Ok(());
                    }

                    bot.send_chat_action(msg.chat.id, ChatAction::Typing).await?;
                    match download_telegram_file(&bot, file_id).await {
                        Ok(data) => match transcriber.transcribe_bytes(filename, data).await {
                            Ok(transcript) if !transcript.is_empty() => {
                                let inbound = InboundMessage {
                                    channel: "telegram".to_string(),
                                    chat_id,
                                    sender_id,
                                    content: transcript,
                                };
                                bus.publish_inbound(inbound).await;
                            }
                            Ok(_) => {
                                bot.send_message(
                                    msg.chat.id,
                                    "I couldn't extract text from that audio message.",
                                )
                                .await?;
                            }
                            Err(err) => {
                                warn!("audio transcription failed: {err}");
                                bot.send_message(
                                    msg.chat.id,
                                    "I couldn't transcribe that audio message. Please retry or send text.",
                                )
                                .await?;
                            }
                        },
                        Err(err) => {
                            warn!("audio download failed: {err}");
                            bot.send_message(
                                msg.chat.id,
                                "I couldn't download that audio message from Telegram.",
                            )
                            .await?;
                        }
                    }
                }

                Ok(())
            }
        });

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![bus])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;

    Ok(())
}

fn is_allowed(msg: &Message, allowlist: &[String]) -> bool {
    if allowlist.is_empty() {
        return true;
    }
    let user = match msg.from.as_ref() {
        Some(u) => u,
        None => return false,
    };
    let uid = user.id.0.to_string();
    let uname = user.username.as_ref().map(|u| u.to_string());
    allowlist.iter().any(|allowed| {
        if allowed.starts_with('@') {
            uname
                .as_ref()
                .map(|u| format!("@{u}"))
                .as_ref()
                .map(|u| u == allowed)
                .unwrap_or(false)
        } else {
            allowed == &uid || uname.as_ref().map(|u| u == allowed).unwrap_or(false)
        }
    })
}

fn spawn_outbound_forwarder(bot: Bot, bus_handle: BusHandle) {
    tokio::spawn(async move {
        loop {
            let next = {
                let mut rx = bus_handle.outbound_rx.lock().await;
                rx.recv().await
            };
            let Some(msg) = next else {
                info!("outbound channel closed, forwarder shutting down");
                break;
            };
            if msg.channel != "telegram" {
                continue;
            }
            if let Ok(chat_id) = msg.chat_id.parse::<i64>() {
                let rendered = markdown_to_telegram_markdown_v2(&msg.content);
                let _ = bot
                    .send_message(ChatId(chat_id), rendered)
                    .parse_mode(ParseMode::MarkdownV2)
                    .await;
            }
        }
    });
}

fn markdown_to_telegram_markdown_v2(input: &str) -> String {
    #[derive(Clone, Copy)]
    enum ListKind {
        Unordered,
        Ordered,
    }

    #[derive(Clone, Copy)]
    struct ListState {
        kind: ListKind,
        next: u64,
    }

    fn ensure_line_break(out: &mut String) {
        if !out.ends_with('\n') && !out.is_empty() {
            out.push('\n');
        }
    }

    fn push_blockquote_prefix(out: &mut String, depth: usize) {
        for _ in 0..depth {
            out.push_str("\\> ");
        }
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(input, options);
    let mut out = String::with_capacity(input.len() + 16);
    let mut list_stack: Vec<ListState> = Vec::new();
    let mut in_code_block = false;
    let mut item_open = false;
    let mut link_urls: Vec<String> = Vec::new();
    let mut blockquote_depth = 0usize;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => {
                    ensure_line_break(&mut out);
                    out.push('*');
                }
                Tag::List(start) => {
                    list_stack.push(ListState {
                        kind: if start.is_some() {
                            ListKind::Ordered
                        } else {
                            ListKind::Unordered
                        },
                        next: start.unwrap_or(1),
                    });
                    ensure_line_break(&mut out);
                }
                Tag::Item => {
                    ensure_line_break(&mut out);
                    if let Some(last) = list_stack.last_mut() {
                        match last.kind {
                            ListKind::Unordered => out.push_str("• "),
                            ListKind::Ordered => {
                                out.push_str(&last.next.to_string());
                                out.push_str("\\. ");
                                last.next += 1;
                            }
                        }
                    }
                    item_open = true;
                }
                Tag::Emphasis => out.push('_'),
                Tag::Strong => out.push('*'),
                Tag::Strikethrough => out.push('~'),
                Tag::BlockQuote(_) => {
                    ensure_line_break(&mut out);
                    blockquote_depth += 1;
                    push_blockquote_prefix(&mut out, blockquote_depth);
                }
                Tag::Link { dest_url, .. } => {
                    out.push('[');
                    link_urls.push(dest_url.to_string());
                }
                Tag::CodeBlock(kind) => {
                    ensure_line_break(&mut out);
                    out.push_str("```");
                    if let CodeBlockKind::Fenced(lang) = kind {
                        let lang = lang.trim();
                        if !lang.is_empty() {
                            out.push_str(&escape_markdown_v2_code(lang));
                        }
                    }
                    out.push('\n');
                    in_code_block = true;
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => {
                    ensure_line_break(&mut out);
                }
                TagEnd::Heading(_) => {
                    out.push('*');
                    ensure_line_break(&mut out);
                }
                TagEnd::List(_) => {
                    let _ = list_stack.pop();
                    ensure_line_break(&mut out);
                }
                TagEnd::Item => {
                    if item_open {
                        ensure_line_break(&mut out);
                    }
                    item_open = false;
                }
                TagEnd::Emphasis => out.push('_'),
                TagEnd::Strong => out.push('*'),
                TagEnd::Strikethrough => out.push('~'),
                TagEnd::Link => {
                    let url = link_urls.pop().unwrap_or_default();
                    out.push(']');
                    out.push('(');
                    out.push_str(&escape_markdown_v2_url(&url));
                    out.push(')');
                }
                TagEnd::CodeBlock => {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str("```");
                    ensure_line_break(&mut out);
                    in_code_block = false;
                }
                TagEnd::BlockQuote(_) => {
                    ensure_line_break(&mut out);
                    blockquote_depth = blockquote_depth.saturating_sub(1);
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_code_block {
                    out.push_str(&escape_markdown_v2_code(&text));
                } else {
                    out.push_str(&escape_markdown_v2_text(&text));
                }
            }
            Event::Code(code) => {
                out.push('`');
                out.push_str(&escape_markdown_v2_code(&code));
                out.push('`');
            }
            Event::InlineHtml(html) | Event::Html(html) => {
                out.push_str(&escape_markdown_v2_text(&html));
            }
            Event::InlineMath(math) | Event::DisplayMath(math) => {
                out.push_str(&escape_markdown_v2_text(&math));
            }
            Event::SoftBreak | Event::HardBreak => {
                out.push('\n');
                if blockquote_depth > 0 {
                    push_blockquote_prefix(&mut out, blockquote_depth);
                }
            }
            Event::Rule => {
                ensure_line_break(&mut out);
                out.push_str("\\-\\-\\-");
                ensure_line_break(&mut out);
            }
            Event::FootnoteReference(label) => {
                out.push('[');
                out.push_str(&escape_markdown_v2_text(&label));
                out.push(']');
            }
            Event::TaskListMarker(checked) => {
                if checked {
                    out.push_str("\\[x\\] ");
                } else {
                    out.push_str("\\[ \\] ");
                }
            }
        }
    }

    out.trim_end().to_string()
}

fn escape_markdown_v2_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        push_escaped_markdown_v2_char(&mut out, ch);
    }
    out
}

fn escape_markdown_v2_code(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '`' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

fn escape_markdown_v2_url(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            ')' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

fn push_escaped_markdown_v2_char(out: &mut String, ch: char) {
    match ch {
        '_' | '*' | '[' | ']' | '(' | ')' | '~' | '`' | '>' | '#' | '+' | '-' | '=' | '|'
        | '{' | '}' | '.' | '!' | '\\' => {
            out.push('\\');
            out.push(ch);
        }
        _ => out.push(ch),
    }
}

async fn download_telegram_file(bot: &Bot, file_id: FileId) -> Result<Vec<u8>> {
    let file = bot.get_file(file_id).await?;
    let mut data = Vec::new();
    bot.download_file(&file.path, &mut data).await?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::markdown_to_telegram_markdown_v2;

    #[test]
    fn renders_multiline_blockquote_lines() {
        let input = "> first line\n> second line";
        let rendered = markdown_to_telegram_markdown_v2(input);
        assert_eq!(rendered, "\\> first line\n\\> second line");
    }
}
