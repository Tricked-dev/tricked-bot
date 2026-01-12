use std::process::{Command, Stdio};

/// Update exchange rates (call at startup, needs network)
pub fn update_rates() -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!("Updating qalc exchange rates...");

    // Use -e flag with a dummy expression to trigger exchange rate update and exit
    // qalc will update exchange rates if needed when -e is used
    let output = Command::new("qalc")
        .args(["-e", "1+1"]) // Execute simple expression and exit
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if !output.success() {
        tracing::warn!("Warning: couldn't update qalc exchange rates");
    } else {
        tracing::info!("Successfully updated qalc exchange rates");
    }
    Ok(())
}

/// Eval expression with 2s timeout, no interactive prompts
pub fn qalc(expr: &str) -> Result<String, String> {
    let output = Command::new("qalc")
        .args([
            "-t",                             // terse output
            "-m", "2000",                     // 2000ms timeout
            "-s", "update exchange rates 0", // never prompt to update
            expr,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("timed out") || stderr.contains("aborted") {
            Err("Calculation timed out (2s limit)".into())
        } else {
            Err(stderr.trim().to_string())
        }
    }
}
