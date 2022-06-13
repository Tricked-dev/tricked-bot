use argh::FromArgs;
use mdcat::{push_tty, Environment, ResourceAccess, Settings, TerminalCapabilities, TerminalSize};
use pulldown_cmark::{Options, Parser};
use qrcodegen::{QrCode, QrCodeEcc};
use rand::prelude::{IteratorRandom, SliceRandom};
use rand::Rng;

use select::document::Document;
use select::predicate::Class;

use std::error::Error;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;
use syntect::parsing::SyntaxSet;
use tokio::sync::MutexGuard;
use twilight_embed_builder::{EmbedAuthorBuilder, EmbedBuilder, ImageSource};
use twilight_http::Client as HttpClient;
use twilight_model::channel::embed::Embed;
use twilight_model::gateway::payload::incoming::MessageCreate;
use twilight_model::http::attachment::Attachment;

use crate::structs::{
    Command, Commands, Config, InviteStats, List, Search, State, TrickedCommands,
};
use crate::zalgos::zalgify_text;
use crate::{print_qr, RESPONDERS};

pub async fn handle_message(
    msg: &MessageCreate,
    mut locked_state: MutexGuard<'_, State>,
    config: &Arc<Config>,
    http: &Arc<HttpClient>,
) -> Result<Command, Box<dyn Error + Send + Sync>> {
    if let Some(responder) = RESPONDERS.get(msg.content.to_uppercase().as_str()) {
        if let Some(msg) = &responder.message {
            return Ok(Command::text(msg));
        }
        if let Some(reaction) = &responder.react {
            return Ok(Command::react(reaction.chars().next().unwrap()));
        }
    }
    if msg.content.to_lowercase().starts_with("l+") {
        let join = msg.content.split('+').skip(1).collect::<Vec<_>>().join("+");
        let args = join.trim().split(' ');
        let two = &args.collect::<Vec<&str>>()[..];
        let commands = TrickedCommands::from_args(&["L+"], two);
        if let Ok(command) = commands {
            return match command.nested {
                Commands::QR(qr) => {
                    let qr = QrCode::encode_text(&qr.text.join(" "), QrCodeEcc::Low)?;
                    let res = print_qr(&qr);
                    let embed = EmbedBuilder::new()
                        .description(format!("```ansi\n{res}\n```"))
                        .build()?;
                    Ok(Command::embed(embed))
                }

                Commands::MD(md) => {
                    let env = &Environment::for_local_directory(&"/")?;
                    let settings = &Settings {
                        resource_access: ResourceAccess::LocalOnly,
                        syntax_set: SyntaxSet::load_defaults_newlines(),
                        terminal_capabilities: TerminalCapabilities::ansi(),
                        terminal_size: TerminalSize::default(),
                    };

                    let mut buf = Vec::new();
                    let text = md.text.join(" ");
                    let parser = Parser::new_ext(
                        &text,
                        Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH,
                    );
                    push_tty(settings, env, &mut buf, parser)?;
                    let res = String::from_utf8(buf.clone())?;
                    let size = res.len();
                    if size > 4050 {
                        Ok(
                            Command::text("Message exceeded discord limit send attachment!")
                                .attachments(vec![Attachment::from_bytes(
                                    "message.ansi".to_string(),
                                    buf.clone(),
                                    125,
                                )]),
                        )
                    } else if size > 2000 {
                        let embed = EmbedBuilder::new()
                            .description(format!("```ansi\n{res}\n```"))
                            .build()?;
                        Ok(Command::embed(embed))
                    } else {
                        Ok(Command::text(format!("```ansi\n{res}\n```")))
                    }
                }
                Commands::Zip(_) => {
                    let size = msg.attachments.iter().map(|x| x.size).sum::<u64>();
                    if size > 6000000 {
                        return Ok(Command::text("Too big".to_string()));
                    }
                    let mut buf = Vec::new();

                    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));

                    let options = zip::write::FileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored);
                    for attachment in &msg.attachments {
                        zip.start_file(attachment.filename.clone(), options)?;
                        zip.write_all(
                            &locked_state
                                .client
                                .get(&attachment.url)
                                .send()
                                .await?
                                .bytes()
                                .await?,
                        )?;
                    }

                    let res = zip.finish()?;

                    Ok(Command::text("Zip files arrived").attachments(vec![
                        Attachment::from_bytes(
                            format!("files-{}.zip", msg.id.get()),
                            (*res.get_ref()).clone(),
                            125,
                        ),
                    ]))
                }
                Commands::InviteStats(InviteStats {}) => {
                    let mut stmt = locked_state
                        .db
                        .prepare("select invite_used, count(invite_used) from users group by invite_used")
                        .unwrap();
                    let mut res = stmt
                        .query_map([], |row| {
                            let key: String = row.get(0)?;
                            let value: i32 = row.get(1)?;
                            Ok((key, value))
                        })?
                        .flatten()
                        .collect::<Vec<(String, i32)>>();

                    res.sort_by(|a, b| b.1.cmp(&a.1));

                    let data = res
                        .into_iter()
                        .map(|(k, v)| {
                            format!(
                                "{k} ({}): {v}",
                                config
                                    .invites
                                    .clone()
                                    .into_iter()
                                    .find_map(|(key, val)| if val == k { Some(key) } else { None })
                                    .unwrap_or_else(|| "None".to_owned())
                            )
                        })
                        .collect::<Vec<String>>();
                    let embed = EmbedBuilder::new().description(data.join("\n")).build()?;
                    Ok(Command::embed(embed))
                }
                Commands::Search(Search { query }) => {
                    let res = locked_state
                        .client
                        .get(format!(
                            "https://duckduckgo.com/html/?q={}",
                            query.join("-")
                        ))
                        .send()
                        .await?
                        .text()
                        .await?;
                    let document = Document::from(res.as_str());
                    let embeds =
                        document
                            .find(Class("result__body"))
                            .take(5)
                            .map(|node| -> Option<Embed> {
                                let url_node = node.find(Class("result__a")).next()?;
                                let url = url_node
                                    .attr("href")?
                                    .replace("//duckduckgo.com", "https://duckduckgo.com");
                                let title = url_node.inner_html();
                                let snippet = node
                                    .find(Class("result__snippet"))
                                    .next()?
                                    .inner_html()
                                    .replace("<b>", "**")
                                    .replace("</b>", "**")
                                    .split_whitespace()
                                    .collect::<Vec<&str>>()
                                    .join(" ");

                                let icon = node
                                    .find(Class("result__icon__img"))
                                    .next()?
                                    .attr("src")?
                                    .replace(
                                        "//external-content.duckduckgo.com",
                                        "https://external-content.duckduckgo.com",
                                    );

                                let preview_url =
                                    node.find(Class("result__url")).next()?.inner_html();
                                EmbedBuilder::new()
                                    .title(title)
                                    .url(url)
                                    .color(0x179e87)
                                    .description(snippet)
                                    .author(
                                        EmbedAuthorBuilder::new(preview_url)
                                            .icon_url(ImageSource::url(icon).ok()?),
                                    )
                                    .build()
                                    .ok()
                            });
                    let embeds: Vec<Embed> = embeds.flatten().collect();
                    Ok(if embeds.is_empty() {
                        Command::text("Nothing found (or i am blocked)")
                    } else {
                        Command::embeds(embeds)
                    })
                }
            };
        } else {
            return Ok(Command::text(format!(
                "```\n{}\n```",
                commands.err().unwrap().output
            )));
        }
    }

    match msg.content.to_lowercase().as_str() {
        content
            if locked_state.last_redesc.elapsed() > std::time::Duration::from_secs(150)
                && config
                    .rename_channels
                    .clone()
                    .contains(&msg.channel_id.get())
                && locked_state.rng.gen_range(0..10) == 2 =>
        {
            if content.to_lowercase().contains("uwu") || content.to_lowercase().contains("owo") {
                Ok(Command::text("No furry shit!!!!!"))
            } else {
                tracing::info!("Channel renamed");
                match http.update_channel(msg.channel_id).topic(content) {
                    Ok(req) => {
                        req.exec().await?;
                        locked_state.last_redesc = Instant::now();
                    }
                    Err(err) => tracing::error!("{:?}", err),
                }
                Ok(Command::nothing())
            }
        }
        x if x.contains("im") => {
            let text = msg.content.split("im").last().unwrap().trim();
            if text.is_empty() {
                return Ok(Command::nothing());
            }

            Ok(Command::text(format!("Hi {text} i'm Tricked-bot")).reply())
        }
        _x if locked_state.rng.gen_range(0..75) == 2 => {
            let content = zalgify_text(locked_state.rng.clone(), msg.content.to_owned());
            Ok(Command::text(content).reply())
        }
        _x if locked_state.rng.gen_range(0..55) == 2 => {
            let mut text = msg.content.split(' ').collect::<Vec<&str>>();
            text.shuffle(&mut locked_state.rng.clone());
            Ok(Command::text(text.join(" ")).reply())
        }
        _ if locked_state.rng.gen_range(0..40) == 2 => {
            let res = locked_state
                .client
                .get(format!(
                    "https://www.reddit.com/r/{}/.json",
                    config.shit_reddits.choose(&mut rand::thread_rng()).unwrap()
                ))
                .send()
                .await?
                .json::<List>()
                .await?
                .data
                .children
                .into_iter()
                .filter(|x| !x.data.over_18)
                .filter(|x| x.data.url_overridden_by_dest.contains("i."))
                .choose(&mut locked_state.rng)
                .map(|x| x.data.url_overridden_by_dest);
            if let Some(pic) = res {
                Ok(Command::text(pic))
            } else {
                Ok(Command::nothing())
            }
        }
        _ => Ok(Command::nothing()),
    }
}
