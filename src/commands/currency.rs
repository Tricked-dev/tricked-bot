#![allow(clippy::unused_unit)]

use std::sync::Arc;

use tokio::sync::Mutex;
use vesper::{
    prelude::*,
    twilight_exports::{InteractionResponse, InteractionResponseData, InteractionResponseType},
};

use crate::structs::State;

#[command]
#[description = "Convert USD to other currencies"]
pub async fn usd(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "Amount in USD"] amount: f64,
) -> DefaultCommandResult {
    let state = ctx.data.lock().await;
    let rates = &state.currency_rates;

    let eur = amount * rates.rates.get("EUR").unwrap_or(&0.92);
    let jpy = amount * rates.rates.get("JPY").unwrap_or(&149.0);
    let pln = amount * rates.rates.get("PLN").unwrap_or(&4.0);

    let message = format!(
        "💵 **${:.2} USD** converts to:\n\n\
         🇪🇺 **€{:.2} EUR**\n\
         🇯🇵 **¥{:.2} JPY**\n\
         🇵🇱 **{:.2} PLN**",
        amount, eur, jpy, pln
    );

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

#[command]
#[description = "Convert EUR to other currencies"]
pub async fn euro(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "Amount in EUR"] amount: f64,
) -> DefaultCommandResult {
    let state = ctx.data.lock().await;
    let rates = &state.currency_rates;

    let eur_rate = rates.rates.get("EUR").unwrap_or(&0.92);
    let usd = amount / eur_rate;
    let jpy = usd * rates.rates.get("JPY").unwrap_or(&149.0);
    let pln = usd * rates.rates.get("PLN").unwrap_or(&4.0);

    let message = format!(
        "🇪🇺 **€{:.2} EUR** converts to:\n\n\
         💵 **${:.2} USD**\n\
         🇯🇵 **¥{:.2} JPY**\n\
         🇵🇱 **{:.2} PLN**",
        amount, usd, jpy, pln
    );

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

#[command]
#[description = "Convert JPY (Yen) to other currencies"]
pub async fn yen(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "Amount in JPY"] amount: f64,
) -> DefaultCommandResult {
    let state = ctx.data.lock().await;
    let rates = &state.currency_rates;

    let jpy_rate = rates.rates.get("JPY").unwrap_or(&149.0);
    let usd = amount / jpy_rate;
    let eur = usd * rates.rates.get("EUR").unwrap_or(&0.92);
    let pln = usd * rates.rates.get("PLN").unwrap_or(&4.0);

    let message = format!(
        "🇯🇵 **¥{:.2} JPY** converts to:\n\n\
         💵 **${:.2} USD**\n\
         🇪🇺 **€{:.2} EUR**\n\
         🇵🇱 **{:.2} PLN**",
        amount, usd, eur, pln
    );

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

#[command]
#[description = "Convert PLN (Polish Złoty) to other currencies"]
pub async fn pln(
    ctx: &SlashContext<'_, Arc<Mutex<State>>>,
    #[description = "Amount in PLN"] amount: f64,
) -> DefaultCommandResult {
    let state = ctx.data.lock().await;
    let rates = &state.currency_rates;

    let pln_rate = rates.rates.get("PLN").unwrap_or(&4.0);
    let usd = amount / pln_rate;
    let eur = usd * rates.rates.get("EUR").unwrap_or(&0.92);
    let jpy = usd * rates.rates.get("JPY").unwrap_or(&149.0);

    let message = format!(
        "🇵🇱 **{:.2} PLN** converts to:\n\n\
         💵 **${:.2} USD**\n\
         🇪🇺 **€{:.2} EUR**\n\
         🇯🇵 **¥{:.2} JPY**",
        amount, usd, eur, jpy
    );

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
