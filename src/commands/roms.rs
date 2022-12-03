use zephyrus::{
    prelude::*,
    twilight_exports::{CommandOptionChoice, InteractionResponse, InteractionResponseData, InteractionResponseType},
};

use crate::roms::{codename, format_device, search};

#[command]
#[description = "Find the fucking rom!"]
pub async fn command(
    ctx: &SlashContext<'_, ()>,
    #[autocomplete = "autocomplete_arg"]
    #[description = "Some description"]
    device: Option<String>,
    #[description = "Some description"] code: Option<String>,
) -> CommandResult {
    let cdn = device.map(Some).unwrap_or(code);
    let m = if let Some(device) = cdn {
        let device = codename(device).await;
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
pub async fn autocomplete_arg(ctx: AutocompleteContext<()>) -> Option<InteractionResponseData> {
    let r = search(ctx.user_input.unwrap()).await;
    Some(InteractionResponseData {
        choices: r.map(|x| {
            let devices = x.1;
            devices
                .into_iter()
                .map(|x| CommandOptionChoice::String {
                    value: x.codename.clone(),
                    name: format!("{} ({}): {}", x.name, x.codename, x.roms.len()),
                    name_localizations: None,
                })
                .collect::<Vec<CommandOptionChoice>>()
        }),
        ..Default::default()
    })
}
