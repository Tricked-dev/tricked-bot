//! "Called it": predictions the bot makes in replies, and the follow-up it posts once they are due.

use std::{collections::HashMap, sync::Arc, time::Duration};

use color_eyre::{Result, eyre::eyre};
use twilight_model::id::Id;

use super::{AiRequest, openrouter::Content, output, prompt, reply::REPLY_TIMEOUT};
use crate::{
    bot::Bot,
    db::{self, Prediction},
};

/// How often due predictions are looked for.
const SWEEP: Duration = Duration::from_secs(60);
/// Follow-ups posted per sweep, so a backlog after downtime trickles out instead of flooding.
const PER_SWEEP: i64 = 5;

/// Remembers `prediction`, made in the reply to `req`, to check back on later.
pub async fn store(bot: &Bot, req: &AiRequest, prediction: &output::Prediction) {
    let stored = db::add_prediction(
        &bot.db,
        req.channel_id.get(),
        req.message_id.get(),
        req.user_id.get(),
        &prediction.claim,
        prediction.wait.as_secs_f64(),
    )
    .await;
    match stored {
        Ok(stored) => {
            tracing::info!(target: "predictions", msg_id = %req.message_id, user = %req.user_id, stored,
                wait_secs = prediction.wait.as_secs(), claim = prediction.claim, "Prediction made");
        }
        Err(error) => {
            tracing::warn!(target: "predictions", msg_id = %req.message_id, "Storing prediction failed: {error}");
        }
    }
}

/// `seconds` as a rough human duration: "3 hours", "2 days", "2 weeks".
fn rough_age(seconds: i64) -> String {
    let (count, unit) = match seconds.max(0) {
        s if s >= 14 * 86_400 => (s / (7 * 86_400), "week"),
        s if s >= 86_400 => (s / 86_400, "day"),
        s => ((s / 3600).max(1), "hour"),
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// The instruction for a follow-up, framed as a note from the server rather than the person's own words.
fn follow_up_note(name: &str, prediction: &Prediction) -> String {
    format!(
        "Server note, not from {name}: {} ago you predicted this about {name}: \"{}\". Check back on it now, in one \
         line aimed at {name}. If the recent conversation shows how it turned out, gloat or cope accordingly; if it \
         doesn't, call it anyway with full \"told you\" confidence. No gif or predict lines.",
        rough_age(prediction.age_secs),
        prediction.claim,
    )
}

/// Posts the follow-up for one due prediction, as a reply to the message the bot was answering when it made it.
async fn follow_up(bot: &Bot, prediction: &Prediction) -> Result<()> {
    let ai = bot.ai.as_ref().ok_or_else(|| eyre!("OpenRouter API key not configured"))?;
    let user_id = Id::new_checked(prediction.user_id.cast_unsigned()).ok_or_else(|| eyre!("invalid user id"))?;
    let channel_id = Id::new_checked(prediction.channel_id.cast_unsigned()).ok_or_else(|| eyre!("invalid channel"))?;
    let message_id = Id::new_checked(prediction.message_id.cast_unsigned()).ok_or_else(|| eyre!("invalid message"))?;
    let user = db::user(&bot.db, user_id.get()).await?.ok_or_else(|| eyre!("predicted user is gone"))?;
    let mut user_mentions = HashMap::from([(user.name.clone(), user_id)]);
    let conversation = bot.transcript(channel_id, Id::new(u64::MAX), &mut user_mentions);
    let req = AiRequest {
        user_id,
        user_name: user.name.clone(),
        channel_id,
        message_id,
        message: follow_up_note(&user.name, prediction),
        media: Vec::new(),
        conversation,
        reply_target: None,
        user_mentions,
    };
    let turn = Content::Text(prompt::text_turn(&req));
    let request = prompt::build_request(&bot.config, &req, Some(&user), &[], turn, &bot.config.openrouter_model)?;
    let text = ai.complete(&request, REPLY_TIMEOUT).await?;
    let text = output::presentable(&text).ok_or_else(|| eyre!("empty follow-up"))?;
    // The original message may be gone by now; the follow-up still makes sense on its own.
    bot.http.create_message(channel_id).content(&text).reply(message_id).fail_if_not_exists(false).await?;
    Ok(())
}

/// Checks for due predictions every [`SWEEP`] and follows up on them.
pub fn spawn(bot: Arc<Bot>) {
    if bot.ai.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SWEEP);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let due = match db::take_due_predictions(&bot.db, PER_SWEEP).await {
                Ok(due) => due,
                Err(error) => {
                    tracing::warn!(target: "predictions", "Loading due predictions failed: {error}");
                    continue;
                }
            };
            for prediction in &due {
                match follow_up(&bot, prediction).await {
                    Ok(()) => {
                        tracing::info!(target: "predictions", user = prediction.user_id, claim = prediction.claim,
                            "Followed up on prediction");
                    }
                    Err(error) => tracing::warn!(target: "predictions", user = prediction.user_id,
                        "Prediction follow-up failed: {error}"),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_round_down_to_the_largest_sensible_unit() {
        assert_eq!(rough_age(0), "1 hour");
        assert_eq!(rough_age(3 * 3600 + 59), "3 hours");
        assert_eq!(rough_age(86_400), "1 day");
        assert_eq!(rough_age(13 * 86_400), "13 days");
        assert_eq!(rough_age(15 * 86_400), "2 weeks");
    }

    #[test]
    fn follow_up_note_names_the_claim_and_forbids_new_directives() {
        let prediction = Prediction {
            channel_id: 1,
            message_id: 2,
            user_id: 3,
            claim: "back on windows".into(),
            age_secs: 14 * 86_400,
        };
        let note = follow_up_note("alice", &prediction);
        assert!(note.contains("2 weeks ago you predicted this about alice: \"back on windows\""));
        assert!(note.contains("No gif or predict lines"));
    }
}
