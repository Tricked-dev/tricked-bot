use std::{collections::HashSet, error::Error, sync::Arc};

use reqwest::Client;
use serde::Deserialize;
use tokio::sync::Mutex;
use twilight_model::application::command::CommandOptionChoiceValue;
use urlencoding::encode;
use vesper::{
    prelude::*,
    twilight_exports::{CommandOptionChoice, InteractionResponse, InteractionResponseData, InteractionResponseType},
};

use crate::structs::State;

#[command]
#[description = "Find the fucking rom!"]
pub async fn roms(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[autocomplete(autocomplete_arg)]
    #[description = "Some description"]
    device: Option<String>,
    #[description = "Some description"] code: Option<String>,
) -> DefaultCommandResult {
    let state = ctx.data.lock().await;
    let cdn = device.map(Some).unwrap_or(code);
    let m = if let Some(device) = cdn {
        let device = codename(&state.client, device).await;
        if let Some(device) = device {
            format_device(device)
        } else {
            "Phone not found".to_owned()
        }
    } else {
        "Please provide either a device or a codename".to_owned()
    };

    ctx.interaction_client
        .create_response(
            ctx.interaction.id,
            &ctx.interaction.token,
            &InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    content: Some(m),
                    ..Default::default()
                }),
            },
        )
        .exec()
        .await?;

    Ok(())
}

#[autocomplete]
pub async fn autocomplete_arg(ctx: AutocompleteContext<Arc<Mutex<State>>>) -> Option<InteractionResponseData> {
    let r = search(&ctx.data.lock().await.client, ctx.user_input.input).await;
    Some(InteractionResponseData {
        choices: r.map(|x| {
            let devices = x.1;
            devices
                .into_iter()
                .map(|x| CommandOptionChoice {
                    value: CommandOptionChoiceValue::String(x.codename.clone()),
                    name: format!("{} ({}): {}", x.name, x.codename, x.roms.len()),
                    name_localizations: None,
                })
                .collect::<Vec<CommandOptionChoice>>()
        }),
        ..Default::default()
    })
}

#[derive(Deserialize, Debug, Clone, Hash, PartialEq, Eq)]
pub struct RomDevice {
    id: String,
}

fn default_resource() -> String {
    "Unknown".to_string()
}

#[derive(Deserialize, Debug, Clone)]
pub struct Device {
    #[serde(default = "default_resource")]
    pub name: String,
    #[serde(default = "default_resource")]
    pub codename: String,
    #[serde(default = "default_resource")]
    pub brand: String,
    pub roms: HashSet<String>,
}

pub async fn req(client: &Client, url: String) -> Result<Vec<Device>, Box<dyn Error + Send + Sync>> {
    Ok(serde_json::from_str(&client.get(url).send().await?.text().await?)?)
}

pub async fn search(client: &Client, text: String) -> Option<(Device, Vec<Device>)> {
    let results = req(
        client,
        format!("https://nowrom.deno.dev/device?q={}&limit=10", encode(&text)),
    )
    .await
    .ok()?;

    if results.is_empty() {
        None
    } else {
        let mut iter = results.into_iter();

        Some((iter.next().unwrap(), iter.collect::<Vec<_>>()))
    }
}

pub async fn codename(client: &Client, i: String) -> Option<Device> {
    let results = req(
        client,
        format!("https://nowrom.deno.dev/device?codename={}", encode(&i)),
    )
    .await
    .ok()?;

    if results.is_empty() {
        None
    } else {
        Some(results[0].clone())
    }
}

pub fn format_device(d: Device) -> String {
    format!("https://rom.tricked.pro/device/{}", d.codename,)
}
