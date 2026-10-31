//! Calculations via the `qalc` CLI from Qalculate!.

use std::{process::Stdio, time::Duration};

use tokio::process::Command;

/// qalc gives up on its own after this long; the process is killed shortly after as a backstop.
const TIMEOUT_MS: u64 = 2000;

/// Update exchange rates. Needs network, so it runs once in the background at startup.
pub async fn update_rates() {
    // Evaluating any expression makes qalc refresh stale exchange rates before exiting.
    let status = Command::new("qalc").args(["-e", "1+1"]).stdout(Stdio::null()).stderr(Stdio::null()).status().await;
    match status {
        Ok(status) if status.success() => tracing::info!("Updated qalc exchange rates"),
        Ok(status) => tracing::warn!("qalc exchange rate update exited with {status}"),
        Err(error) => tracing::warn!("Failed to run qalc to update exchange rates: {error}"),
    }
}

/// Evaluates `expr` tersely without interactive prompts.
pub async fn eval(expr: &str) -> Result<String, String> {
    let output = Command::new("qalc")
        .args(["-t", "-m", &TIMEOUT_MS.to_string(), "-s", "update exchange rates 0", expr])
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_millis(TIMEOUT_MS + 1000), output)
        .await
        .map_err(|_| "Calculation timed out (2s limit)".to_owned())?
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("timed out") || stderr.contains("aborted") {
        Err("Calculation timed out (2s limit)".into())
    } else {
        Err(stderr.trim().to_owned())
    }
}
