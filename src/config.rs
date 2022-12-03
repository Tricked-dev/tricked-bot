use std::{collections::HashMap, io};

use clap::Parser;
use serde::{Deserialize, Serialize};

#[derive(Parser, Clone, Debug, Serialize, Deserialize, Default)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    #[arg(short, long, env)]
    pub token: String,
    #[arg(short, long, env)]
    pub discord: u64,
    #[arg(short, long, env)]
    pub join_channel: u64,
    #[arg(short, long, env, default_value = "trickedbot.sqlite")]
    pub database_file: String,
    #[arg(short, long, env, default_value = "0")]
    pub id: u64,
    #[arg(long, env)]
    pub rename_channels: Vec<u64>,
    #[arg(long, env, value_parser = parse_invites)]
    pub invites: HashMap<String, String>,
    #[arg(short, long, env, value_parser = parse_invites)]
    pub responders: HashMap<String, String>,
    #[arg(long, env)]
    pub shit_reddits: Vec<String>,
    #[arg(short, long, env, default_value = "I am tricked bot!")]
    pub status: String,
}

fn parse_invites(src: &str) -> Result<HashMap<String, String>, io::Error> {
    let mut map = HashMap::new();
    for pair in src.split(',') {
        let (key, value) = match pair.split_once(':') {
            Some(v) => v,
            None => return Err(io::Error::new(io::ErrorKind::Other, "Invalid invite format")),
        };
        map.insert(key.to_string(), value.parse().unwrap());
    }
    Ok(map)
}
