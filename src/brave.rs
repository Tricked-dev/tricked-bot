use reqwest::Client;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct BraveSearchResult {
    pub title: String,
    pub url: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BraveApiResponse {
    pub web: Option<WebResults>,
}

#[derive(Debug, Deserialize)]
pub struct WebResults {
    pub results: Vec<BraveSearchResult>,
}

#[derive(Clone, Debug)]
pub struct BraveApi {
    client: Client,
    api_key: String,
}

impl BraveApi {
    pub fn new(client: Client, api_key: &str) -> Self {
        Self {
            client,
            api_key: api_key.to_owned(),
        }
    }

    pub async fn search(&self, query: &str) -> Result<Vec<BraveSearchResult>, reqwest::Error> {
        let url = "https://api.search.brave.com/res/v1/web/search";
        let resp = self
            .client
            .get(url)
            .header("Accept", "application/json")
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", query)])
            .send()
            .await?
            .json::<BraveApiResponse>()
            .await?;

        Ok(resp.web.and_then(|w| Some(w.results)).unwrap_or_default())
    }
}
