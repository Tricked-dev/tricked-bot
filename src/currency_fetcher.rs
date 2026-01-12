use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::structs::CurrencyRates;

#[derive(Debug, Deserialize, Serialize)]
struct ExchangeRateResponse {
    result: String,
    base_code: String,
    rates: HashMap<String, f64>,
    time_last_update_unix: u64,
}

/// Fetches currency exchange rates from exchangerate-api.com (free tier, no API key required)
/// Base currency is USD
pub async fn fetch_currency_rates(client: &Client) -> Result<CurrencyRates, Box<dyn std::error::Error>> {
    tracing::info!("Fetching currency exchange rates from API...");

    // Using exchangerate-api.com free tier endpoint (no API key required)
    // This endpoint provides USD-based rates
    let url = "https://open.er-api.com/v6/latest/USD";

    let response = client
        .get(url)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(format!("API returned status: {}", response.status()).into());
    }

    let data: ExchangeRateResponse = response.json().await?;

    if data.result != "success" {
        return Err("API did not return success status".into());
    }

    tracing::info!(
        "Successfully fetched {} currency rates. Base: {}",
        data.rates.len(),
        data.base_code
    );

    Ok(CurrencyRates {
        rates: data.rates,
        base: data.base_code,
        last_updated: data.time_last_update_unix,
    })
}

/// Alternative: Fetches from frankfurter.app (European Central Bank data, also free)
#[allow(dead_code)]
pub async fn fetch_currency_rates_ecb(client: &Client) -> Result<CurrencyRates, Box<dyn std::error::Error>> {
    tracing::info!("Fetching currency exchange rates from ECB API...");

    let url = "https://api.frankfurter.app/latest?from=USD";

    let response = client
        .get(url)
        .send()
        .await?;

    #[derive(Deserialize)]
    struct FrankfurterResponse {
        base: String,
        date: String,
        rates: HashMap<String, f64>,
    }

    let data: FrankfurterResponse = response.json().await?;

    tracing::info!(
        "Successfully fetched {} currency rates from ECB. Base: {}, Date: {}",
        data.rates.len(),
        data.base,
        data.date
    );

    // Add USD to the rates since it's the base
    let mut rates = data.rates;
    rates.insert("USD".to_string(), 1.0);

    Ok(CurrencyRates {
        rates,
        base: data.base,
        last_updated: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    })
}
