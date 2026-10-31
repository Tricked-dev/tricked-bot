//! Exchange rates for the currency conversion commands.

use std::{collections::HashMap, fmt::Write, sync::Arc, time::Duration};

use color_eyre::{Result, eyre::bail};
use serde::Deserialize;

use crate::bot::Bot;

/// A currency the conversion commands support, with a fallback rate per US dollar for when the API is down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Currency {
    pub code: &'static str,
    flag: &'static str,
    symbol: &'static str,
    fallback_rate: f64,
}

pub const USD: Currency = Currency { code: "USD", flag: "💵", symbol: "$", fallback_rate: 1.0 };
pub const EUR: Currency = Currency { code: "EUR", flag: "🇪🇺", symbol: "€", fallback_rate: 0.92 };
pub const JPY: Currency = Currency { code: "JPY", flag: "🇯🇵", symbol: "¥", fallback_rate: 149.0 };
pub const PLN: Currency = Currency { code: "PLN", flag: "🇵🇱", symbol: "", fallback_rate: 4.0 };
pub const CAD: Currency = Currency { code: "CAD", flag: "🇨🇦", symbol: "C$", fallback_rate: 1.36 };

const CURRENCIES: [Currency; 5] = [USD, EUR, JPY, PLN, CAD];

/// Rates per US dollar.
#[derive(Debug, Clone, Default)]
pub struct CurrencyRates(HashMap<String, f64>);

impl CurrencyRates {
    fn per_usd(&self, currency: Currency) -> f64 {
        self.0.get(currency.code).copied().unwrap_or(currency.fallback_rate)
    }

    /// A message converting `amount` of `from` into every other supported currency.
    pub fn convert(&self, amount: f64, from: Currency) -> String {
        let usd = amount / self.per_usd(from);
        let mut message = format!("{} **{}{amount:.2} {}** converts to:\n", from.flag, from.symbol, from.code);
        for to in CURRENCIES.into_iter().filter(|to| *to != from) {
            let converted = usd * self.per_usd(to);
            let _ = write!(message, "\n{} **{}{converted:.2} {}**", to.flag, to.symbol, to.code);
        }
        message
    }
}

#[derive(Deserialize)]
struct ExchangeRateResponse {
    result: String,
    rates: HashMap<String, f64>,
}

/// Fetches USD-based rates from open.er-api.com (free, no API key required).
async fn fetch(client: &reqwest::Client) -> Result<CurrencyRates> {
    let data: ExchangeRateResponse =
        client.get("https://open.er-api.com/v6/latest/USD").send().await?.error_for_status()?.json().await?;
    if data.result != "success" {
        bail!("exchange rate API returned {:?}", data.result);
    }
    Ok(CurrencyRates(data.rates))
}

/// Keeps the bot's rates fresh. The API itself only updates daily.
pub fn spawn_refresh(bot: Arc<Bot>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(6 * 60 * 60));
        loop {
            interval.tick().await;
            match fetch(&bot.web).await {
                Ok(rates) => {
                    tracing::info!("Loaded {} currency exchange rates", rates.0.len());
                    *bot.currency.write().unwrap() = rates;
                }
                Err(error) => tracing::warn!("Failed to fetch currency rates: {error}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_through_usd_into_every_other_currency() {
        let rates = CurrencyRates(HashMap::from([("EUR".into(), 0.5), ("JPY".into(), 100.0)]));
        assert_eq!(
            rates.convert(10.0, EUR),
            "🇪🇺 **€10.00 EUR** converts to:\n\n💵 **$20.00 USD**\n🇯🇵 **¥2000.00 JPY**\n🇵🇱 **80.00 PLN**\n🇨🇦 **C$27.20 CAD**"
        );
    }
}
