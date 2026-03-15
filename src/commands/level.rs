#![allow(clippy::unused_unit)]

use std::sync::Arc;

use crate::db;
use tokio::sync::Mutex;

use twilight_model::id::{marker::UserMarker, Id};
use vesper::{
    prelude::*,
    twilight_exports::{InteractionResponse, InteractionResponseData, InteractionResponseType},
};

use crate::{database::User, structs::State, utils::levels::xp_required_for_level};

#[command]
#[description = "Level "]
pub async fn level(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "The user to level up"] user: Option<Id<UserMarker>>,
) -> DefaultCommandResult {
    let id = user
        .unwrap_or(ctx.interaction.member.clone().unwrap().user.unwrap().id)
        .get();
    let state = ctx.data.lock().await;

    let user = db::get_user(&state.db, id).await?;

    let user = match user {
        Some(user) => user,
        None => {
            println!("nulL!");
            return Ok(());
        }
    };

    let all_users = db::get_all_users_ranked(&state.db).await?;

    let pos = all_users.into_iter().position(|x| x.id as u64 == id);

    let xp_to_next_level = xp_required_for_level(user.level);
    let xp_earned = user.xp;
    let percent = (xp_earned as f64 / xp_to_next_level as f64) * 100.0;

    let bar_count = 20;
    let bar = "█";
    let empty_bar = "░";
    let bar = format!(
        "{}{}",
        bar.repeat((percent / 100.0 * bar_count as f64) as usize),
        empty_bar.repeat((bar_count as f64 - (percent / 100.0 * bar_count as f64)) as usize)
    );
    let message = format!(
        "Level: {}, position: {}\nXP: {xp_earned}/{xp_to_next_level}\n{bar}",
        user.level,
        pos.unwrap_or_default() + 1,
    );
    tracing::info!("Level {message}");
    ctx.interaction_client
        .create_response(
            ctx.interaction.id,
            &ctx.interaction.token,
            &InteractionResponse {
                kind: InteractionResponseType::ChannelMessageWithSource,
                data: Some(InteractionResponseData {
                    content: Some(message),
                    ..Default::default()
                }),
            },
        )
        .exec()
        .await?;

    Ok(())
}
