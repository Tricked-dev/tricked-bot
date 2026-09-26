use std::{collections::HashMap, time::Duration};

use color_eyre::{eyre::eyre, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::Config;

#[derive(Serialize, Clone, Debug)]
pub struct Question {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: String,
    criteria: Value,
}

impl Question {
    pub fn noul(instructions: impl Into<String>, yes: &str, no: &str) -> Self {
        Self {
            kind: "noul",
            instructions: instructions.into(),
            criteria: json!({ "true": yes, "false": no }),
        }
    }
}

#[derive(Deserialize)]
struct Answer {
    noul: Option<f32>,
}

#[derive(Deserialize)]
struct Response {
    answers: HashMap<String, Answer>,
}

#[derive(Clone)]
pub struct Decider {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Decider {
    pub fn new(url: String, token: String) -> Result<Self> {
        let endpoint = reqwest::Url::parse(&url)?;
        if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host_str().is_none() {
            return Err(eyre!("decider URL must be HTTP(S) with a host"));
        }
        Ok(Self {
            http: reqwest::Client::builder().build()?,
            url,
            token,
        })
    }

    pub fn from_config(cfg: &Config) -> Option<Self> {
        Self::new(cfg.decider_url.clone()?, cfg.decider_api_key.clone()?).ok()
    }

    pub async fn noul(
        &self,
        state: &Value,
        questions: HashMap<String, Question>,
        packed: bool,
        timeout: Duration,
    ) -> Result<HashMap<String, f32>> {
        let ids: Vec<String> = questions.keys().cloned().collect();
        let resp: Response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.token)
            .timeout(timeout)
            .json(&json!({ "state": state, "questions": questions, "independent": !packed }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        ids.into_iter()
            .map(|id| {
                let p = resp
                    .answers
                    .get(&id)
                    .and_then(|a| a.noul)
                    .ok_or_else(|| eyre!("decider returned no noul for {id}"))?;
                if !p.is_finite() || !(0.0..=1.0).contains(&p) {
                    return Err(eyre!("decider returned invalid probability for {id}"));
                }
                Ok((id, p))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::State as AxState, http::HeaderMap, routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    async fn serve(delay: Duration) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route(
                "/v1/systemone",
                post(
                    move |AxState(seen): AxState<Seen>, headers: HeaderMap, Json(body): Json<Value>| async move {
                        let auth = headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_string();
                        seen.lock().unwrap().push((auth, body));
                        tokio::time::sleep(delay).await;
                        Json(json!({ "model": "m", "answers": { "q1": { "type": "noul", "noul": 0.75 } } }))
                    },
                ),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/v1/systemone"), seen)
    }

    fn one() -> HashMap<String, Question> {
        HashMap::from([("q1".to_string(), Question::noul("Is it?", "yes", "no"))])
    }

    #[tokio::test]
    async fn parses_noul_and_sends_packed_flag() {
        let (url, seen) = serve(Duration::ZERO).await;
        let d = Decider::new(url, "t0k".into()).unwrap();
        let p = d.noul(&json!("s"), one(), true, Duration::from_secs(2)).await.unwrap();
        assert_eq!(p["q1"], 0.75);
        let (auth, body) = seen.lock().unwrap()[0].clone();
        assert_eq!(auth, "Bearer t0k");
        assert_eq!(body["independent"], false);
        assert_eq!(body["questions"]["q1"]["type"], "noul");
    }

    #[tokio::test]
    async fn times_out() {
        let (url, _) = serve(Duration::from_secs(3)).await;
        let d = Decider::new(url, "t0k".into()).unwrap();
        assert!(d
            .noul(&json!("s"), one(), false, Duration::from_millis(200))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn missing_answer_is_an_error() {
        let (url, _) = serve(Duration::ZERO).await;
        let d = Decider::new(url, "t0k".into()).unwrap();
        let qs = HashMap::from([("other".to_string(), Question::noul("?", "y", "n"))]);
        assert!(d.noul(&json!("s"), qs, false, Duration::from_secs(2)).await.is_err());
    }
    #[tokio::test]
    async fn independent_mode_and_criteria_match_wire_contract() {
        let (url, seen) = serve(Duration::ZERO).await;
        Decider::new(url, "test".into())
            .unwrap()
            .noul(&json!({"message": "hello"}), one(), false, Duration::from_secs(2))
            .await
            .unwrap();
        let body = &seen.lock().unwrap()[0].1;
        assert_eq!(body["independent"], true);
        assert_eq!(body["state"]["message"], "hello");
        assert_eq!(
            body["questions"]["q1"]["criteria"],
            json!({"true": "yes", "false": "no"})
        );
    }

    #[tokio::test]
    async fn malformed_and_invalid_probabilities_fail_closed() {
        for response in [
            json!({"answers": {"q1": {"noul": 1.01}}}),
            json!({"answers": {"q1": {"noul": -0.1}}}),
            json!({"answers": {"q1": {"noul": null}}}),
            json!({"answers": {"q1": {"noul": "yes"}}}),
            json!({"unexpected": true}),
        ] {
            let app = Router::new().route(
                "/",
                post(move || {
                    let response = response.clone();
                    async move { Json(response) }
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            assert!(Decider::new(url, "test".into())
                .unwrap()
                .noul(&json!("s"), one(), false, Duration::from_secs(2))
                .await
                .is_err());
            server.abort();
        }
    }
}
