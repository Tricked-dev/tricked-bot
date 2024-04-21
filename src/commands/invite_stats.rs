#![allow(clippy::unused_unit)]

use std::sync::Arc;

use tokio::sync::Mutex;

use vesper::prelude::*;

use crate::structs::State;

#[command]
#[description = "View invite stats!"]
pub async fn invite_stats(_ctx: &SlashContext<'_, Arc<Mutex<State>>>) -> DefaultCommandResult {
    // let data = {
    //     let state = ctx.data.lock().await;
    //     let mut stmt = state
    //         .db
    //         .prepare("select invite_used, count(invite_used) from users group by invite_used")
    //         .unwrap();
    //     let mut res = stmt
    //         .query_map([], |row| {
    //             let key: String = row.get(0)?;
    //             let value: i32 = row.get(1)?;
    //             Ok((key, value))
    //         })?
    //         .flatten()
    //         .collect::<Vec<(String, i32)>>();

    //     res.sort_by(|a, b| b.1.cmp(&a.1));

    //     res.into_iter()
    //         .map(|(k, v)| {
    //             format!(
    //                 "{k} ({}): {v}",
    //                 state
    //                     .config
    //                     .invites
    //                     .clone()
    //                     .into_iter()
    //                     .find_map(|(key, val)| if val == k { Some(key) } else { None })
    //                     .unwrap_or_else(|| "None".to_owned())
    //             )
    //         })
    //         .collect::<Vec<String>>()
    //         .join("\n")
    // };

    // let embed = Embed {
    //     description: Some(if data.is_empty() {
    //         "No invites used".to_owned()
    //     } else {
    //         data
    //     }),
    //     author: None,
    //     color: Some(0x00ff00),
    //     fields: vec![],
    //     footer: None,
    //     image: None,
    //     kind: "rich".to_owned(),
    //     provider: None,
    //     thumbnail: None,
    //     timestamp: None,
    //     title: None,
    //     url: None,
    //     video: None,
    // };

    // ctx.interaction_client
    //     .create_response(
    //         ctx.interaction.id,
    //         &ctx.interaction.token,
    //         &InteractionResponse {
    //             kind: InteractionResponseType::ChannelMessageWithSource,
    //             data: Some(InteractionResponseData {
    //                 embeds: Some(vec![embed]),
    //                 ..Default::default()
    //             }),
    //         },
    //     )
    //     .exec()
    //     .await?;

    Ok(())
}
