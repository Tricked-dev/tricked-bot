#![allow(clippy::unused_unit)]

use std::sync::Arc;

use num_format::{Locale, ToFormattedString};
use serde_rusqlite::{from_row, from_rows};
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

    let user = {
        let db = state.db.get()?;
        let mut statement = db.prepare("SELECT * FROM user WHERE id = ?").unwrap();
        statement
            .query_row([id], |row| {
                from_row::<User>(row).map_err(|_| rusqlite::Error::QueryReturnedNoRows)
            })
            .ok()
    };

    let user = match user {
        Some(user) => user,
        None => {
            println!("nulL!");
            return Ok(());
        }
    };

    let all_users = {
        let db = state.db.get()?;
        let mut statement = db
            .prepare("SELECT * FROM user WHERE level != 0 ORDER BY level DESC")
            .unwrap();
        from_rows::<User>(statement.query([]).unwrap())
            .flatten()
            .collect::<Vec<User>>()
    };

    let pos = all_users.into_iter().position(|x| x.id == id);

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
        "Level: {}, position: {}\nXP: {xp_earned}/{xp_to_next_level}\n{bar}\nSocial Credit: {}",
        user.level,
        pos.unwrap_or_default() + 1,
        user.social_credit.to_formatted_string(&Locale::en),
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
