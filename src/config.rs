use std::{collections::HashMap, io, num::ParseIntError, path::PathBuf, sync::Arc};

use clap::Parser;

#[derive(Parser, Clone, Debug, Default)]
#[command(author, version, about, long_about = None)]
pub struct Config {
    #[arg(short, long, env)]
    pub token: String,
    #[arg(short, long, env)]
    pub discord: u64,
    #[arg(short, long, env)]
    pub join_channel: u64,
    #[arg(long, env, value_parser = vec_u64_parser)]
    pub message_indicator_channels: Arc<Vec<u64>>,
    #[arg(long, env, default_value = "trickedbot.sqlite")]
    pub database_file: PathBuf,
    #[arg(short, long, env, default_value = "0")]
    pub id: u64,
    #[arg(long, env, value_parser(vec_u64_parser))]
    pub rename_channels: Arc<Vec<u64>>,
    #[arg(long, env, value_parser = parse_invites)]
    pub invites: HashMap<String, String>,
    #[arg(short, long, env, value_parser = parse_invites)]
    pub responders: HashMap<String, String>,
    #[arg(long, env, value_parser = parse_str_array)]
    pub shit_reddits: Arc<Vec<String>>,
    #[arg(short, long, env, default_value = "I am tricked bot!")]
    pub status: String,
    #[arg(short, long, env)]
    pub openai_api_key: Option<String>,
    #[arg(long, env)]
    pub openrouter_api_key: Option<String>,
    #[arg(long, env, default_value = "https://openrouter.ai/api/v1")]
    pub openrouter_base_url: String,
    #[arg(long, env, default_value = "tngtech/deepseek-r1t2-chimera:free")]
    pub openrouter_model: String,
    #[arg(long, env)]
    pub openrouter_site_url: Option<String>,
    #[arg(long, env)]
    pub openrouter_site_name: Option<String>,
    #[arg(long, env)]
    pub today_i_channel: Option<u64>,
    #[arg(long, env)]
    pub brave_api: Option<String>,
}

fn parse_str_array(src: &str) -> Result<Arc<Vec<String>>, io::Error> {
    Ok(Arc::new(src.split(',').map(|x| x.to_owned()).collect()))
}

fn parse_invites(src: &str) -> Result<HashMap<String, String>, io::Error> {
    let mut map = HashMap::new();
    for pair in src.split(',') {
        let (key, value) = match pair.split_once(':') {
            Some(v) => v,
            None => return Err(io::Error::other("Invalid invite format")),
        };
        map.insert(key.to_string(), value.parse().unwrap());
    }
    Ok(map)
}
fn vec_u64_parser(src: &str) -> Result<Arc<Vec<u64>>, ParseIntError> {
    let mut vec = Vec::new();
    for pair in src.split(',') {
        vec.push(pair.parse()?);
    }
    Ok(Arc::new(vec))
}
