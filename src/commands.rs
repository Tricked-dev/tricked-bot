//! Slash commands, registered in the configured guild.

use color_eyre::{Result, eyre::eyre};
use twilight_interactions::command::{CommandInputData, CommandModel, CreateCommand, ResolvedUser};
use twilight_model::{
    application::interaction::{Interaction, InteractionData},
    channel::message::MessageFlags,
    http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
    id::Id,
};

use crate::{
    bot::Bot,
    currency::{self, Currency},
    db, levels, qalc,
};

#[derive(CommandModel, CreateCommand)]
#[command(name = "level", desc = "Show someone's level")]
struct Level {
    /// The user to show the level of
    user: Option<ResolvedUser>,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "qalc", desc = "Calculate mathematical expressions using Qalculate!")]
struct Qalc {
    /// Mathematical expression to calculate
    expression: String,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "translate", desc = "Translate text between languages (uses Google Translate API)")]
struct Translate {
    /// Source language code (en, es, fr, de, pl, ja, zh, ru, it, pt, auto)
    from: String,
    /// Target language code (en, es, fr, de, pl, ja, zh, ru, it, pt)
    to: String,
    /// Text to translate
    text: String,
}

/// One conversion command per currency, all sharing [`currency::CurrencyRates::convert`].
macro_rules! currency_commands {
    ($($command:ident = $name:literal, $currency:path, $desc:literal;)*) => {
        $(
            #[derive(CommandModel, CreateCommand)]
            #[command(name = $name, desc = $desc)]
            struct $command {
                #[command(desc = "Amount to convert")]
                amount: f64,
            }
        )*

        /// Parses a currency command into its amount and source currency.
        fn currency_command(name: &str, data: CommandInputData<'_>) -> Option<Result<(f64, Currency)>> {
            Some(match name {
                $($name => $command::from_interaction(data).map(|cmd| (cmd.amount, $currency)).map_err(Into::into),)*
                _ => return None,
            })
        }

        fn currency_definitions() -> impl Iterator<Item = twilight_model::application::command::Command> {
            [$($command::create_command().into()),*].into_iter()
        }
    };
}

currency_commands! {
    Usd = "usd", currency::USD, "Convert USD to other currencies";
    Euro = "euro", currency::EUR, "Convert EUR to other currencies";
    Yen = "yen", currency::JPY, "Convert JPY (Yen) to other currencies";
    Pln = "pln", currency::PLN, "Convert PLN (Polish Złoty) to other currencies";
    Cad = "cad", currency::CAD, "Convert CAD (Canadian Dollar) to other currencies";
}

pub async fn register(bot: &Bot) -> Result<()> {
    let commands: Vec<_> =
        [Level::create_command().into(), Qalc::create_command().into(), Translate::create_command().into()]
            .into_iter()
            .chain(currency_definitions())
            .collect();
    bot.http.interaction(bot.application_id).set_guild_commands(Id::new(bot.config.discord), &commands).await?;
    Ok(())
}

/// What a command answers with; errors are shown only to the person who ran it.
struct Reply {
    content: String,
    ephemeral: bool,
}

impl From<String> for Reply {
    fn from(content: String) -> Self {
        Self { content, ephemeral: false }
    }
}

pub async fn handle(bot: &Bot, interaction: Interaction) -> Result<()> {
    let Some(InteractionData::ApplicationCommand(data)) = &interaction.data else {
        return Ok(());
    };
    let name = data.name.clone();
    let input = CommandInputData::from(*data.clone());
    let reply = match run(bot, &interaction, &name, input).await {
        Ok(reply) => reply,
        Err(error) => {
            tracing::warn!(command = name, "Slash command failed: {error:?}");
            Reply { content: format!("Something went wrong: {error}"), ephemeral: true }
        }
    };
    let response = InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(InteractionResponseData {
            content: Some(reply.content),
            flags: reply.ephemeral.then_some(MessageFlags::EPHEMERAL),
            ..Default::default()
        }),
    };
    bot.http.interaction(bot.application_id).create_response(interaction.id, &interaction.token, &response).await?;
    Ok(())
}

async fn run(bot: &Bot, interaction: &Interaction, name: &str, input: CommandInputData<'_>) -> Result<Reply> {
    if let Some(command) = currency_command(name, input.clone()) {
        let (amount, from) = command?;
        return Ok(bot.currency.read().unwrap().convert(amount, from).into());
    }
    match name {
        "level" => {
            let Level { user } = Level::from_interaction(input)?;
            let id = match user {
                Some(user) => user.resolved.id,
                None => interaction.author_id().ok_or_else(|| eyre!("interaction has no author"))?,
            };
            level(bot, id.get()).await
        }
        "qalc" => {
            let Qalc { expression } = Qalc::from_interaction(input)?;
            Ok(qalc::answer(&expression).await.into())
        }
        "translate" => translate(bot, Translate::from_interaction(input)?).await.map(Reply::from),
        _ => Err(eyre!("unknown command {name}")),
    }
}

async fn level(bot: &Bot, user_id: u64) -> Result<Reply> {
    let Some(user) = db::user(&bot.db, user_id).await? else {
        return Ok(Reply { content: "No level yet, send some messages first.".into(), ephemeral: true });
    };
    let rank = db::rank(&bot.db, user.level).await?;
    let required = levels::xp_required_for_level(user.level);
    let bar = levels::progress_bar(user.xp, required);
    Ok(format!("Level: {}, position: {rank}\nXP: {}/{required}\n{bar}", user.level, user.xp).into())
}

async fn translate(bot: &Bot, Translate { from, to, text }: Translate) -> Result<String> {
    // Google Translate's free endpoint; returns nested arrays of [translated, original, ...] segments.
    let json: serde_json::Value = bot
        .web
        .get("https://translate.googleapis.com/translate_a/single")
        .query(&[("client", "gtx"), ("sl", &from), ("tl", &to), ("dt", "t"), ("q", &text)])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let translated: String =
        json[0].as_array().into_iter().flatten().filter_map(|segment| segment[0].as_str()).collect();
    let translated = if translated.is_empty() { "Translation failed or returned empty result" } else { &translated };
    let from = if from == "auto" { "Auto Detected".to_owned() } else { from.to_uppercase() };
    Ok(format!(
        "🌐 **Translation** ({from} → {})\n\n**Original:**\n{text}\n\n**Translated:**\n{translated}",
        to.to_uppercase()
    ))
}
