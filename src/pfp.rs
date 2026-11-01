//! Daily profile picture from a random image posted in a channel.

use std::{sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use color_eyre::{
    Result,
    eyre::{bail, eyre},
};
use rand::seq::SliceRandom;
use reqwest::StatusCode;
use twilight_model::id::{
    Id,
    marker::{ChannelMarker, MessageMarker},
};

use crate::{ai::media::image_extension, bot::Bot, ffmpeg, time::unix_now};

const DAY: Duration = Duration::from_hours(24);
/// Attachments in old messages expire, so a few candidates are tried before giving up.
const ATTEMPTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Image {
    url: String,
    /// The message holding this attachment, deleted once the attachment has expired.
    attachment_of: Option<Id<MessageMarker>>,
}

impl Image {
    fn is_gif(&self) -> bool {
        self.url.contains("tenor.com") || image_extension(&self.url) == Some("gif")
    }
}

/// Discord CDN, Tenor, or anything ending in an image extension.
fn is_image_url(url: &str) -> bool {
    ["cdn.discordapp.com", "media.discordapp.net", "tenor.com"].iter().any(|host| url.contains(host))
        || image_extension(url).is_some()
}

async fn channel_images(bot: &Bot, channel: Id<ChannelMarker>) -> Result<Vec<Image>> {
    let messages = bot.http.channel_messages(channel).limit(100).await?.models().await?;
    let images = messages.iter().flat_map(|message| {
        let attachments = message
            .attachments
            .iter()
            .filter(|attachment| is_image_url(&attachment.url))
            .map(|attachment| Image { url: attachment.url.clone(), attachment_of: Some(message.id) });
        let link = message
            .content
            .split_whitespace()
            .find(|word| is_image_url(word))
            .map(|url| Image { url: url.to_owned(), attachment_of: None });
        attachments.chain(link)
    });
    Ok(images.collect())
}

/// Tenor share pages link the actual GIF in their `og:image` meta tag.
async fn resolve_tenor_url(client: &reqwest::Client, url: &str) -> Result<String> {
    const OG_IMAGE: &str = r#"<meta property="og:image" content=""#;
    let html = client.get(url).send().await?.error_for_status()?.text().await?;
    let start = html
        .find(OG_IMAGE)
        .map(|start| start + OG_IMAGE.len())
        .or_else(|| html.find("https://media.tenor.com"))
        .ok_or_else(|| eyre!("no media URL in Tenor page {url}"))?;
    let end = html[start..].find('"').ok_or_else(|| eyre!("unterminated media URL in Tenor page {url}"))?;
    Ok(html[start..start + end].to_owned())
}

async fn download(bot: &Bot, channel: Id<ChannelMarker>, image: &Image) -> Result<Vec<u8>> {
    let url = if image.url.contains("tenor.com") && !image.url.contains("media.tenor.com") {
        resolve_tenor_url(&bot.web, &image.url).await?
    } else {
        image.url.clone()
    };
    let response = bot.web.get(&url).send().await?;
    if response.status() == StatusCode::NOT_FOUND
        && let Some(message) = image.attachment_of
    {
        tracing::warn!(%message, "Attachment URL expired, deleting its message");
        let _ = bot.http.delete_message(channel, message).await;
    }
    Ok(response.error_for_status()?.bytes().await?.to_vec())
}

/// Center-crops to a square with ffmpeg, keeping GIFs animated and turning everything else into PNG.
async fn crop_to_square(image: Vec<u8>, gif: bool) -> Result<Vec<u8>> {
    const CROP: &str = r"crop=min(iw\,ih):min(iw\,ih):(iw-min(iw\,ih))/2:(ih-min(iw\,ih))/2";
    let output: &[&str] = if gif {
        &["-vf", CROP, "-f", "gif"]
    } else {
        &["-vf", CROP, "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png"]
    };
    ffmpeg::run(image, &[], output).await
}

/// Updates the bot's avatar with a random image from the channel.
pub async fn update(bot: &Bot, channel: Id<ChannelMarker>) -> Result<()> {
    let mut images = channel_images(bot, channel).await?;
    if images.is_empty() {
        tracing::warn!(%channel, "No images found for a profile picture");
        return Ok(());
    }
    images.shuffle(&mut rand::rng());
    for image in images.iter().take(ATTEMPTS) {
        let bytes = match download(bot, channel, image).await {
            Ok(bytes) => bytes,
            Err(error) => {
                tracing::warn!(url = image.url, "Profile picture download failed, trying another: {error}");
                continue;
            }
        };
        let gif = image.is_gif();
        let cropped = crop_to_square(bytes, gif).await?;
        let mime = if gif { "image/gif" } else { "image/png" };
        let avatar = format!("data:{mime};base64,{}", STANDARD.encode(cropped));
        bot.http.update_current_user().avatar(Some(&avatar)).await?;
        tracing::info!(url = image.url, "Updated profile picture");
        return Ok(());
    }
    bail!("Failed to update profile picture after {ATTEMPTS} attempts")
}

/// Changes the profile picture every midnight UTC, and right away when configured.
pub fn spawn_daily(bot: Arc<Bot>) {
    let Some(channel) = bot.config.pfp_channel.map(Id::new) else {
        tracing::info!("Profile picture channel not configured, skipping daily updates");
        return;
    };
    tokio::spawn(async move {
        if bot.config.pfp_on_startup
            && let Err(error) = update(&bot, channel).await
        {
            tracing::error!("Failed to update profile picture on startup: {error:?}");
        }
        let since_midnight = unix_now().as_secs() % DAY.as_secs();
        let first = tokio::time::Instant::now() + DAY - Duration::from_secs(since_midnight);
        let mut interval = tokio::time::interval_at(first, DAY);
        loop {
            interval.tick().await;
            if let Err(error) = update(&bot, channel).await {
                tracing::error!("Failed to update profile picture: {error:?}");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_urls_are_cdn_tenor_or_image_extensions() {
        assert!(is_image_url("https://cdn.discordapp.com/attachments/1/2/a.mp4"));
        assert!(is_image_url("https://tenor.com/view/cat-123"));
        assert!(is_image_url("https://example.com/a.webp"));
        assert!(!is_image_url("https://example.com/page"));
        assert!(is_image_url("https://example.com/a.PNG?size=large"));
    }
}
