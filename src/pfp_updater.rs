use color_eyre::Result;
use rand::Rng;
use reqwest::Client;
use std::io::Write;
use std::sync::Arc;
use tempfile::NamedTempFile;
use tokio::process::Command;
use tokio::sync::Mutex;
use twilight_http::Client as HttpClient;
use twilight_model::{channel::message::Message, id::Id};

use crate::structs::State;

/// Image info with URL and optional message/channel IDs for refreshing
#[derive(Clone)]
pub struct ImageInfo {
    pub url: String,
    pub channel_id: Option<u64>,
    pub message_id: Option<u64>,
}

/// Fetches messages from a channel and extracts image URLs
pub async fn fetch_images_from_channel(http: &Arc<HttpClient>, channel_id: u64) -> Result<Vec<ImageInfo>> {
    let mut images = Vec::new();

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
                images.push(ImageInfo {
                    url: attachment.url.clone(),
                    channel_id: Some(channel_id),
                    message_id: Some(message.id.get()),
                });
            }
        }

        // Extract URLs from message content
        if let Some(url) = extract_url_from_content(&message) {
            if is_valid_image_url(&url) {
                images.push(ImageInfo {
                    url,
                    channel_id: None,
                    message_id: None,
                });
            }
        }
    }

    tracing::info!("Found {} images in channel {}", images.len(), channel_id);
    Ok(images)
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
async fn download_image(
    client: &Client,
    url: &str,
    http: Option<&Arc<HttpClient>>,
    image_info: Option<&ImageInfo>,
) -> Result<Vec<u8>> {
    let download_url = if url.contains("tenor.com") && !url.contains("media.tenor.com") {
        // Handle Tenor share URLs by extracting the direct media URL
        resolve_tenor_url(client, url).await?
    } else {
        url.to_string()
    };

    let response = client.get(&download_url).send().await;

    // Check if download failed
    match response {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                let bytes = resp.bytes().await?;
                Ok(bytes.to_vec())
            } else if status.as_u16() == 404 {
                // Delete expired Discord attachments
                if let (Some(http), Some(info)) = (http, image_info) {
                    if let (Some(channel_id), Some(message_id)) = (info.channel_id, info.message_id) {
                        tracing::warn!(
                            "Attachment URL expired (404), deleting message {} from channel {}",
                            message_id,
                            channel_id
                        );
                        let _ = http
                            .delete_message(Id::new(channel_id), Id::new(message_id))
                            .exec()
                            .await;
                        return Err(color_eyre::eyre::eyre!("Attachment expired and deleted"));
                    }
                }
                Err(color_eyre::eyre::eyre!("Download failed with 404: URL expired"))
            } else {
                Err(color_eyre::eyre::eyre!("Download failed with status: {}", status))
            }
        }
        Err(e) => Err(color_eyre::eyre::eyre!("Download request failed: {}", e)),
    }
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

/// Crops an image to a 1:1 aspect ratio (centered) using ffmpeg
async fn crop_to_square(image_bytes: Vec<u8>, is_gif: bool) -> Result<Vec<u8>> {
    // Create temporary files with proper extensions
    let extension = if is_gif { "gif" } else { "png" };

    let mut input_file = NamedTempFile::with_suffix(format!(".{}", extension))?;
    input_file.write_all(&image_bytes)?;
    input_file.flush()?;

    // Keep the file around by persisting it temporarily
    let (_, input_path) = input_file.keep()?;

    let output_file = NamedTempFile::with_suffix(format!(".{}", extension))?;
    let (_, output_path) = output_file.keep()?;

    let input_path_str = input_path
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("Invalid input path"))?;
    let output_path_str = output_path
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("Invalid output path"))?;
    dbg!(&output_path);
    dbg!(&input_path);
    // Use ffmpeg to crop to square (centered)
    // The crop filter uses: crop=out_w:out_h:x:y
    // We use min(iw,ih) for both width and height to get a square
    // And center it with (iw-min(iw,ih))/2 and (ih-min(iw,ih))/2
    let output = Command::new("ffmpeg")
        .args([
            "-i",
            input_path_str,
            "-vf",
            "crop=min(iw\\,ih):min(iw\\,ih):(iw-min(iw\\,ih))/2:(ih-min(iw\\,ih))/2",
            "-y",
            output_path_str,
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::error!("ffmpeg failed: {}", stderr);
        // Clean up
        let _ = tokio::fs::remove_file(&input_path).await;
        let _ = tokio::fs::remove_file(&output_path).await;
        return Err(color_eyre::eyre::eyre!("ffmpeg failed to crop image"));
    }

    // Read the output file
    let cropped_bytes = tokio::fs::read(&output_path).await?;

    // Clean up temporary files
    let _ = tokio::fs::remove_file(&input_path).await;
    let _ = tokio::fs::remove_file(&output_path).await;

    tracing::info!("Successfully cropped image to square using ffmpeg");
    Ok(cropped_bytes)
}

/// Updates the bot's profile picture with a random image from the channel
pub async fn update_profile_picture(http: &Arc<HttpClient>, state: &Arc<Mutex<State>>, channel_id: u64) -> Result<()> {
    tracing::info!("Starting profile picture update from channel {}", channel_id);

    // Retry up to 5 times in case we hit expired attachments
    for attempt in 0..5 {
        if attempt > 0 {
            tracing::info!("Retry attempt {} after expired attachment", attempt);
        }

        // Fetch all images from the channel
        let images = fetch_images_from_channel(http, channel_id).await?;

        if images.is_empty() {
            tracing::warn!("No images found in channel {}", channel_id);
            return Ok(());
        }

        // Select a random image
        let selected_image: ImageInfo = {
            let mut locked_state = state.lock().await;
            let index = locked_state.rng.gen_range(0..images.len());
            images[index].clone()
        };

        tracing::info!("Selected image: {}", selected_image.url);

        // Download the image
        let image_bytes = {
            let locked_state = state.lock().await;
            match download_image(
                &locked_state.client,
                &selected_image.url,
                Some(http),
                Some(&selected_image),
            )
            .await
            {
                Ok(bytes) => bytes,
                Err(e) => {
                    tracing::warn!("Failed to download image: {}, retrying with different image", e);
                    continue; // Try again with a different image
                }
            }
        };

        // Determine the image format
        let is_gif = selected_image.url.contains("tenor.com") || selected_image.url.ends_with(".gif");
        let image_format = if is_gif {
            "image/gif"
        } else if selected_image.url.ends_with(".png") {
            "image/png"
        } else if selected_image.url.ends_with(".webp") {
            "image/webp"
        } else {
            "image/jpeg"
        };

        // Crop the image to a 1:1 aspect ratio
        let cropped_bytes = crop_to_square(image_bytes, is_gif).await?;

        // Create data URI
        let base64_image = base64::encode(&cropped_bytes);
        let data_uri = format!("data:{};base64,{}", image_format, base64_image);

        // Update the bot's avatar
        http.update_current_user().avatar(Some(&data_uri)).exec().await?;

        tracing::info!("Successfully updated profile picture!");
        return Ok(());
    }

    Err(color_eyre::eyre::eyre!(
        "Failed to update profile picture after 5 attempts"
    ))
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
