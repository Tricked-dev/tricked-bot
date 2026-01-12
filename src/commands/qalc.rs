#![allow(clippy::unused_unit)]

use std::sync::Arc;

use tokio::sync::Mutex;
use vesper::{
    prelude::*,
    twilight_exports::{InteractionResponse, InteractionResponseData, InteractionResponseType},
};

use crate::structs::State;

#[command]
#[description = "Calculate mathematical expressions using Qalculate!"]
pub async fn qalc(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "Mathematical expression to calculate"] expression: String,
) -> DefaultCommandResult {
    // Run qalc in a blocking task since it's a system command
    let result = tokio::task::spawn_blocking(move || crate::qalc::qalc(&expression)).await;

    let message = match result {
        Ok(Ok(output)) => {
            if output.is_empty() {
                "No result returned from calculation".to_string()
            } else {
                output
            }
        }
        Ok(Err(e)) => format!("Error: {}", e),
        Err(e) => format!("Failed to execute calculation: {}", e),
    };

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
        .await?;

    Ok(())
}
