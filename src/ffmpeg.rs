//! Running ffmpeg on untrusted image bytes.

use std::{io::Write, time::Duration};

use color_eyre::{
    Result,
    eyre::{bail, eyre},
};
use tokio::process::Command;

/// ffmpeg is killed once it has run this long.
const TIMEOUT: Duration = Duration::from_secs(15);

/// Runs ffmpeg on `input` and returns what it wrote to stdout. `input_args` go before `-i` and `output_args` after it.
///
/// The input goes through a temporary file because some containers (MP4) need a seekable input. Decoding is limited to
/// local files, a single thread and 64 MiB per allocation.
pub async fn run(input: Vec<u8>, input_args: &[&str], output_args: &[&str]) -> Result<Vec<u8>> {
    let file = tokio::task::spawn_blocking(move || {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&input)?;
        Ok::<_, std::io::Error>(file)
    })
    .await??;
    let mut command = Command::new("ffmpeg");
    command
        .kill_on_drop(true)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-max_alloc", "67108864"])
        .args(["-protocol_whitelist", "file,pipe", "-threads", "1"])
        .args(input_args)
        .arg("-i")
        .arg(file.path())
        .args(["-filter_threads", "1", "-threads", "1"])
        .args(output_args)
        .arg("pipe:1");
    let output = tokio::time::timeout(TIMEOUT, command.output()).await.map_err(|_| eyre!("ffmpeg timed out"))??;
    if !output.status.success() || output.stdout.is_empty() {
        bail!("ffmpeg failed: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(output.stdout)
}
