use color_eyre::Result;
use rand::Rng;
use reqwest::Client;
use std::sync::Arc;
use tokio::sync::Mutex;
use twilight_http::Client as HttpClient;
use twilight_model::{channel::message::Message, id::Id};

use crate::structs::State;

/// Fetches messages from a channel and extracts image URLs
pub async fn fetch_images_from_channel(http: &Arc<HttpClient>, channel_id: u64) -> Result<Vec<String>> {
    let mut image_urls = Vec::new();

    // Fetch messages (we'll just get the latest 100)
    let messages = match http.channel_messages(Id::new(channel_id)).limit(100) {
        Ok(request) => match request.exec().await {
            Ok(response) => match response.models().await {
                Ok(msgs) => msgs,
                Err(e) => {
                    tracing::error!("Failed to parse messages: {:?}", e);
                    return Ok(Vec::new());
                }
            },
            Err(e) => {
                tracing::error!("Failed to fetch messages: {:?}", e);
                return Ok(Vec::new());
            }
        },
        Err(e) => {
            tracing::error!("Failed to create request: {:?}", e);
            return Ok(Vec::new());
        }
    };

    for message in messages {
        // Extract attachment URLs
        for attachment in &message.attachments {
            if is_valid_image_url(&attachment.url) {
                image_urls.push(attachment.url.clone());
            }
        }

        // Extract URLs from message content
        if let Some(url) = extract_url_from_content(&message) {
            if is_valid_image_url(&url) {
                image_urls.push(url);
            }
        }
    }

    tracing::info!("Found {} images in channel {}", image_urls.len(), channel_id);
    Ok(image_urls)
}

/// Checks if a URL is a valid image URL (Discord CDN or Tenor)
fn is_valid_image_url(url: &str) -> bool {
    url.contains("cdn.discordapp.com")
        || url.contains("media.discordapp.net")
        || url.contains("tenor.com")
        || url.ends_with(".png")
        || url.ends_with(".jpg")
        || url.ends_with(".jpeg")
        || url.ends_with(".gif")
        || url.ends_with(".webp")
}

/// Extracts image URLs from message content
fn extract_url_from_content(message: &Message) -> Option<String> {
    let content = &message.content;

    // Simple URL extraction - looks for Discord CDN, Tenor, or image URLs
    for word in content.split_whitespace() {
        if is_valid_image_url(word) {
            return Some(word.to_string());
        }
    }

    None
}

/// Downloads an image and returns the bytes
async fn download_image(client: &Client, url: &str) -> Result<Vec<u8>> {
    let download_url = if url.contains("tenor.com") && !url.contains("media.tenor.com") {
        // Handle Tenor share URLs by extracting the direct media URL
        resolve_tenor_url(client, url).await?
    } else {
        url.to_string()
    };

    let response = client.get(&download_url).send().await?;
    let bytes = response.bytes().await?;
    Ok(bytes.to_vec())
}

/// Resolves a Tenor share URL to a direct media URL
async fn resolve_tenor_url(client: &Client, url: &str) -> Result<String> {
    // Fetch the Tenor page
    let response = client.get(url).send().await?;
    let html = response.text().await?;

    // Try to find the GIF URL in meta tags (og:image or twitter:image)
    if let Some(start) = html.find(r#"<meta property="og:image" content=""#) {
        let content_start = start + r#"<meta property="og:image" content=""#.len();
        if let Some(end) = html[content_start..].find('"') {
            let gif_url = &html[content_start..content_start + end];
            tracing::info!("Resolved Tenor URL: {} -> {}", url, gif_url);
            return Ok(gif_url.to_string());
        }
    }

    // Fallback: try to find media.tenor.com URLs directly in the HTML
    if let Some(start) = html.find("https://media.tenor.com") {
        if let Some(end) = html[start..].find('"') {
            let gif_url = &html[start..start + end];
            tracing::info!("Resolved Tenor URL: {} -> {}", url, gif_url);
            return Ok(gif_url.to_string());
        }
    }

    tracing::warn!("Could not resolve Tenor URL: {}", url);
    Err(color_eyre::eyre::eyre!("Failed to resolve Tenor URL"))
}

/// Updates the bot's profile picture with a random image from the channel
pub async fn update_profile_picture(http: &Arc<HttpClient>, state: &Arc<Mutex<State>>, channel_id: u64) -> Result<()> {
    tracing::info!("Starting profile picture update from channel {}", channel_id);

    // Fetch all images from the channel
    let image_urls = fetch_images_from_channel(http, channel_id).await?;

    if image_urls.is_empty() {
        tracing::warn!("No images found in channel {}", channel_id);
        return Ok(());
    }

    // Select a random image
    let selected_url: String = {
        let mut locked_state = state.lock().await;
        let index = locked_state.rng.gen_range(0..image_urls.len());
        image_urls[index].clone()
    };

    if selected_url.is_empty() {
        tracing::warn!("Failed to select random image");
        return Ok(());
    }

    tracing::info!("Selected image: {}", selected_url);

    // Download the image
    let image_bytes = {
        let locked_state = state.lock().await;
        download_image(&locked_state.client, &selected_url).await?
    };

    // Determine the image format
    let image_format = if selected_url.contains("tenor.com") || selected_url.ends_with(".gif") {
        "image/gif"
    } else if selected_url.ends_with(".png") {
        "image/png"
    } else if selected_url.ends_with(".webp") {
        "image/webp"
    } else {
        "image/jpeg"
    };

    // Create data URI
    let base64_image = base64::encode(&image_bytes);
    let data_uri = format!("data:{};base64,{}", image_format, base64_image);
    // dbg!(data_uri.len());
    // dbg!(selected_url);
    // Update the bot's avatar
    http.update_current_user().avatar(Some(&data_uri)).exec().await?;

    tracing::info!("Successfully updated profile picture!");
    Ok(())
}

/// Schedules daily profile picture updates
pub async fn schedule_daily_updates(http: Arc<HttpClient>, state: Arc<Mutex<State>>) {
    let channel_id = {
        let locked_state = state.lock().await;
        locked_state.config.pfp_channel
    };

    let Some(channel_id) = channel_id else {
        tracing::info!("Profile picture channel not configured, skipping daily updates");
        return;
    };

    tokio::spawn(async move {
        let now = std::time::SystemTime::now();
        let since_epoch = now.duration_since(std::time::UNIX_EPOCH).unwrap();

        let seconds_today = since_epoch.as_secs() % (24 * 60 * 60);
        let seconds_until_midnight = (24 * 60 * 60) - seconds_today;
        let duration_until_midnight = std::time::Duration::from_secs(seconds_until_midnight);

        tracing::info!(
            "Waiting {} seconds until midnight for first profile picture update",
            seconds_until_midnight
        );

        // Wait until midnight
        tokio::time::sleep(duration_until_midnight).await;

        loop {
            if let Err(e) = update_profile_picture(&http, &state, channel_id).await {
                tracing::error!("Failed to update profile picture: {:?}", e);
            }

            // Wait 24 hours until next midnight
            tokio::time::sleep(tokio::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}
