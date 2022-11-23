use std::collections::HashSet;

use lazy_static::lazy_static;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use urlencoding::encode;

static APP_USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),);

lazy_static! {
    pub static ref CLIENT: Client = Client::builder().user_agent(APP_USER_AGENT).build().unwrap();
}

#[derive(Deserialize, Serialize, Debug, Clone, Hash, PartialEq, Eq)]
pub struct RomDevice {
    id: String,
}

fn default_resource() -> String {
    "Unknown".to_string()
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Device {
    #[serde(default = "default_resource")]
    pub name: String,
    #[serde(default = "default_resource")]
    pub codename: String,
    #[serde(default = "default_resource")]
    pub brand: String,
    pub roms: HashSet<String>,
}

pub async fn req(url: String) -> Vec<Device> {
    serde_json::from_str(&CLIENT.get(url).send().await.unwrap().text().await.unwrap()).unwrap()
}

pub async fn search(text: String) -> Option<(Device, Vec<Device>)> {
    let results = req(format!("https://nowrom.deno.dev/device?q={}&limit=10", encode(&text))).await;

    if results.is_empty() {
        None
    } else {
        let mut iter = results.into_iter();

        Some((iter.next().unwrap(), iter.collect::<Vec<_>>()))
    }
}

pub async fn codename(i: String) -> Option<Device> {
    let results = req(format!("https://nowrom.deno.dev/device?codename={}", encode(&i))).await;

    if results.is_empty() {
        None
    } else {
        Some(results[0].clone())
    }
}

pub fn format_device(d: Device) -> String {
    format!("https://rom.tricked.pro/device/{}", d.codename,)
}
