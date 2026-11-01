//! Reaction GIFs for replies: any search through Klipy when a key is configured, otherwise a fixed set of anime
//! reactions from nekos.best, which needs no key.

use std::time::Duration;

use color_eyre::{Result, eyre::eyre};
use rand::seq::IndexedRandom;
use serde::Deserialize;
use twilight_model::channel::message::{Embed, embed::EmbedImage};

const TIMEOUT: Duration = Duration::from_secs(5);
const NEKOS: &str = "https://nekos.best/api/v2";
/// nekos.best blocks requests whose user agent doesn't name the app and a way to reach its owner.
const NEKOS_USER_AGENT: &str = "tricked-bot (https://github.com/Tricked-dev/tricked-bot)";
const KLIPY: &str = "https://api.klipy.com/api/v1";
/// Picking among the top few results keeps repeated searches from always posting the same GIF.
const KLIPY_CANDIDATES: u8 = 8;

/// The reactions nekos.best has GIFs for, from its `/endpoints` listing.
pub const REACTIONS: &[&str] = &[
    "angry",
    "baka",
    "bite",
    "bleh",
    "blush",
    "bonk",
    "bored",
    "clap",
    "confused",
    "cry",
    "dance",
    "facepalm",
    "happy",
    "highfive",
    "kick",
    "laugh",
    "lurk",
    "nod",
    "nope",
    "pat",
    "pout",
    "punch",
    "run",
    "salute",
    "shake",
    "shocked",
    "shoot",
    "shrug",
    "sip",
    "slap",
    "sleep",
    "smile",
    "smug",
    "stare",
    "tableflip",
    "teehee",
    "think",
    "thumbsup",
    "wave",
    "wink",
    "yawn",
    "yeet",
];

#[derive(Deserialize)]
struct NekosResponse {
    results: Vec<NekosGif>,
}

#[derive(Deserialize)]
struct NekosGif {
    url: String,
}

#[derive(Deserialize)]
struct KlipyResponse {
    data: KlipyPage,
}

#[derive(Deserialize)]
struct KlipyPage {
    data: Vec<KlipyGif>,
}

#[derive(Deserialize)]
struct KlipyGif {
    file: KlipySizes,
}

#[derive(Deserialize)]
struct KlipySizes {
    md: Option<KlipyFormats>,
    hd: Option<KlipyFormats>,
}

#[derive(Deserialize)]
struct KlipyFormats {
    gif: Option<KlipyFile>,
}

#[derive(Deserialize)]
struct KlipyFile {
    url: String,
}

/// The URL of a GIF for `query`, or `None` when there is none. With a Klipy key `query` is a free search; without one
/// it must name one of the [`REACTIONS`].
pub async fn find(client: &reqwest::Client, klipy_key: Option<&str>, query: &str) -> Result<Option<String>> {
    match klipy_key {
        Some(key) => klipy(client, key, query).await,
        None => nekos(client, query).await,
    }
}

async fn nekos(client: &reqwest::Client, reaction: &str) -> Result<Option<String>> {
    let reaction = reaction.trim().to_ascii_lowercase();
    if !REACTIONS.contains(&reaction.as_str()) {
        return Ok(None);
    }
    let response: NekosResponse = client
        .get(format!("{NEKOS}/{reaction}"))
        .header(reqwest::header::USER_AGENT, NEKOS_USER_AGENT)
        .timeout(TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    Ok(response.results.into_iter().next().map(|gif| gif.url))
}

async fn klipy(client: &reqwest::Client, key: &str, query: &str) -> Result<Option<String>> {
    let per_page = KLIPY_CANDIDATES.to_string();
    let response: KlipyResponse = client
        .get(format!("{KLIPY}/{key}/gifs/search"))
        .query(&[("q", query), ("per_page", &per_page)])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|error| eyre!("Klipy search failed: {}", error.without_url()))?
        .error_for_status()
        .map_err(|error| eyre!("Klipy search failed: {}", error.without_url()))?
        .json()
        .await?;
    Ok(klipy_urls(response).choose(&mut rand::rng()).cloned())
}

/// Each result's GIF URL, medium size where there is one.
fn klipy_urls(response: KlipyResponse) -> Vec<String> {
    response
        .data
        .data
        .into_iter()
        .filter_map(|gif| [gif.file.md, gif.file.hd].into_iter().flatten().find_map(|sizes| sizes.gif))
        .map(|file| file.url)
        .collect()
}

/// An embed that shows just the GIF.
pub fn embed(url: String) -> Embed {
    Embed {
        author: None,
        color: None,
        description: None,
        fields: Vec::new(),
        footer: None,
        image: Some(EmbedImage { height: None, proxy_url: None, url, width: None }),
        kind: "rich".into(),
        provider: None,
        thumbnail: None,
        timestamp: None,
        title: None,
        url: None,
        video: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn klipy_results_prefer_the_medium_gif() {
        let response: KlipyResponse = serde_json::from_value(serde_json::json!({
            "result": true,
            "data": {"data": [
                {"id": 1, "file": {"hd": {"gif": {"url": "hd.gif"}}, "md": {"gif": {"url": "md.gif"}}}},
                {"id": 2, "file": {"hd": {"gif": {"url": "only-hd.gif"}}}},
                {"id": 3, "file": {"xs": {"jpg": {"url": "thumb.jpg"}}}}
            ], "has_next": false}
        }))
        .unwrap();
        assert_eq!(klipy_urls(response), ["md.gif", "only-hd.gif"]);
    }

    #[tokio::test]
    #[ignore = "calls the live nekos.best API"]
    async fn nekos_serves_a_reaction_without_a_key() {
        let url = find(&reqwest::Client::new(), None, "Smug").await.unwrap().unwrap();
        assert!(url.starts_with("https://nekos.best/api/v2/smug/"), "{url}");
    }

    #[tokio::test]
    async fn unknown_reactions_never_hit_the_network() {
        let client = reqwest::Client::new();
        assert_eq!(nekos(&client, "bro is cooked").await.unwrap(), None);
    }
}
