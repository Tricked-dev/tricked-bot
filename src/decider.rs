use std::{collections::HashMap, sync::Arc, time::Duration};

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
    probabilities: Option<HashMap<String, f32>>,
}

#[derive(Deserialize)]
struct Response {
    answers: HashMap<String, Answer>,
}

pub struct ChoiceResult {
    pub probabilities: HashMap<String, f32>,
    pub flags: HashMap<String, f32>,
}

fn validate_probability(p: f32) -> Result<f32> {
    if !p.is_finite() || !(0.0..=1.0).contains(&p) {
        return Err(eyre!("invalid choice probability"));
    }
    Ok(p)
}

fn validate_choice(
    options: &HashMap<String, String>,
    probabilities: HashMap<String, f32>,
) -> Result<HashMap<String, f32>> {
    if probabilities.len() != options.len() || options.keys().any(|id| !probabilities.contains_key(id)) {
        return Err(eyre!("choice response does not match requested options"));
    }
    for p in probabilities.values() {
        validate_probability(*p)?;
    }
    let sum: f32 = probabilities.values().sum();
    // Wire probabilities are rounded to four decimals, including 255-option lists.
    if (sum - 1.0).abs() > 0.02 {
        return Err(eyre!("choice probabilities are not normalized"));
    }
    Ok(probabilities)
}

#[derive(Clone)]
pub struct Decider {
    http: reqwest::Client,
    url: String,
    token: String,
    in_flight: Arc<tokio::sync::Semaphore>,
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
            in_flight: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    pub fn from_config(cfg: &Config) -> Option<Self> {
        Self::new(cfg.decider_url.clone()?, cfg.decider_api_key.clone()?).ok()
    }

    pub async fn choice(
        &self,
        state: &Value,
        instructions: &str,
        options: HashMap<String, String>,
        flags: HashMap<String, Question>,
        timeout: Duration,
    ) -> Result<ChoiceResult> {
        if !(2..=255).contains(&options.len()) || flags.contains_key("selection") {
            return Err(eyre!("choice requires 2..255 options and a reserved selection id"));
        }
        tokio::time::timeout(timeout, async {
            let _permit = self.in_flight.acquire().await?;
            let mut qs: HashMap<String, Value> = flags
                .iter()
                .map(|(id, q)| Ok((id.clone(), serde_json::to_value(q)?)))
                .collect::<Result<_>>()?;
            qs.insert(
                "selection".into(),
                json!({"type":"choice", "instructions":instructions, "criteria":options}),
            );
            let mut response: Response = self
                .http
                .post(&self.url)
                .bearer_auth(&self.token)
                .timeout(timeout)
                .json(&json!({"state":state,"questions":qs,"independent":false}))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let probabilities = response
                .answers
                .remove("selection")
                .and_then(|a| a.probabilities)
                .ok_or_else(|| eyre!("missing choice probabilities"))?;
            let probabilities = validate_choice(&options, probabilities)?;
            let flags = flags
                .keys()
                .map(|id| {
                    let p = response
                        .answers
                        .get(id)
                        .and_then(|a| a.noul)
                        .ok_or_else(|| eyre!("missing flag {id}"))?;
                    Ok((id.clone(), validate_probability(p)?))
                })
                .collect::<Result<_>>()?;
            Ok(ChoiceResult { probabilities, flags })
        })
        .await?
    }

    pub async fn noul(
        &self,
        state: &Value,
        questions: HashMap<String, Question>,
        packed: bool,
        timeout: Duration,
    ) -> Result<HashMap<String, f32>> {
        // Independent questions can be split without changing their meaning. Large
        // recall batches exhaust the Mac worker; serialize small batches across clones.
        // Queueing and all batches share one deadline, not a fresh timeout per batch.
        tokio::time::timeout(timeout, async {
            let batch_size = if packed { questions.len().max(1) } else { 8 };
            let mut remaining = questions.into_iter();
            let mut scores = HashMap::new();
            loop {
                let batch: HashMap<_, _> = remaining.by_ref().take(batch_size).collect();
                if batch.is_empty() {
                    break;
                }
                let _permit = self.in_flight.acquire().await?;
                scores.extend(self.noul_batch(state, batch, packed, timeout).await?);
            }
            Ok::<_, color_eyre::Report>(scores)
        })
        .await?
    }

    async fn noul_batch(
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
    async fn independent_questions_are_bounded_and_all_answers_preserved() {
        let seen: Arc<Mutex<Vec<usize>>> = Arc::default();
        let recorded = seen.clone();
        let app = Router::new().route(
            "/",
            post(move |Json(body): Json<Value>| {
                let seen = recorded.clone();
                async move {
                    let qs = body["questions"].as_object().unwrap();
                    seen.lock().unwrap().push(qs.len());
                    // Model the backend's failure on oversized independent batches.
                    if qs.len() > 8 {
                        return (axum::http::StatusCode::PAYLOAD_TOO_LARGE, Json(json!({})));
                    }
                    let answers: serde_json::Map<String, Value> =
                        qs.keys().map(|id| (id.clone(), json!({"noul": 0.75}))).collect();
                    (axum::http::StatusCode::OK, Json(json!({"answers": answers})))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let qs = (0..19)
            .map(|i| (format!("q{i}"), Question::noul("?", "y", "n")))
            .collect();
        let result = Decider::new(url, "test".into())
            .unwrap()
            .noul(&json!("s"), qs, false, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(result.len(), 19);
        assert_eq!(*seen.lock().unwrap(), vec![8, 8, 3]);
        server.abort();
    }

    #[tokio::test]
    async fn choice_sends_names_and_parses_all_weights_and_flags() {
        let app = Router::new().route(
            "/",
            post(|Json(body): Json<Value>| async move {
                assert_eq!(body["independent"], false);
                assert_eq!(body["questions"]["selection"]["type"], "choice");
                assert_eq!(
                    body["questions"]["selection"]["criteria"],
                    json!({"m1":"hobbies","m2":"work"})
                );
                assert_eq!(body["questions"].as_object().unwrap().len(), 2);
                Json(json!({"answers":{"selection":{"probabilities":{"m1":0.7,"m2":0.3}},"durable":{"noul":0.8}}}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let d = Decider::new(url, "test".into()).unwrap();
        let result = d
            .choice(
                &json!("hi"),
                "rank",
                HashMap::from([("m1".into(), "hobbies".into()), ("m2".into(), "work".into())]),
                HashMap::from([("durable".into(), Question::noul("?", "y", "n"))]),
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(result.probabilities["m1"], 0.7);
        assert_eq!(result.flags["durable"], 0.8);
        let too_many = (0..256).map(|i| (i.to_string(), "name".into())).collect();
        assert!(d
            .choice(&json!("s"), "rank", too_many, HashMap::new(), Duration::from_secs(1))
            .await
            .is_err());
        server.abort();
    }

    #[test]
    fn malformed_choice_weights_fail_closed_including_missing_and_unknown_ids() {
        let options = HashMap::from([("a".into(), "one".into()), ("b".into(), "two".into())]);
        for weights in [
            HashMap::from([("a".into(), 1.0)]),
            HashMap::from([("a".into(), 0.5), ("other".into(), 0.5)]),
            HashMap::from([("a".into(), f32::NAN), ("b".into(), 0.5)]),
            HashMap::from([("a".into(), 0.1), ("b".into(), 0.1)]),
            HashMap::from([("a".into(), -0.1), ("b".into(), 1.1)]),
        ] {
            assert!(validate_choice(&options, weights).is_err());
        }
        let options = (0..255).map(|i| (i.to_string(), "name".into())).collect();
        let rounded = (0..255).map(|i| (i.to_string(), 0.0039)).collect();
        assert!(validate_choice(&options, rounded).is_ok());
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
