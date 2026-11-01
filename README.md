# Tricked-Bot

A simple discord bot made for my [discord](https://discord.gg/mY8zTARu4g)

## Features

* The Trickster: an AI regular that replies when pinged (and, optionally, when it has something to add), with
  per-user memories, image understanding, and embed text from messages, replies, and recent history. It can attach reaction GIFs (anime reactions from nekos.best with no
  key, or any GIF search with `KLIPY_API_KEY`; `DISABLE_GIFS` turns them off) and comes back to gloat about its
  predictions once they're due
* Incoming recorded voice messages are transcribed once and saved; the Trickster can add a separate spoken punchline
  or send a voice-only reply when delivery fits the moment
* Unprompted Community Notes on confidently wrong claims, and rulings or polls when an argument drags on
* XP and levels, plus surprise math and colour quizzes
* Slash commands: `/level`, `/qalc`, `/translate` and currency conversion (`/usd`, `/euro`, `/yen`, `/pln`, `/cad`)
* Responders, `t!ratewaifu`, a rotating channel topic and a daily profile picture
* A small web UI for editing users and memories (`WEB_PORT`)

## Running

Configuration comes from flags or environment variables (a `.env` file is loaded); see `tricked-bot --help`.
`TOKEN`, `DISCORD` (the guild to register slash commands in) and `DATABASE_URL` are the essentials.

Replies default to `anthropic/claude-haiku-5.5`; memory extraction defaults to `qwen/qwen3.5-9b`.
Override them with `OPENROUTER_MODEL` and `OPENROUTER_MEMORY_MODEL`. Existing environment values take precedence
over these defaults, so change `OPENROUTER_MEMORY_MODEL` explicitly when switching an existing deployment.

Speech uses OpenRouter too: `OPENROUTER_TTS_MODEL` defaults to `fish-audio/s1-20260729`, and
`OPENROUTER_STT_MODEL` to `mistralai/voxtral-small-transcribe-3-0`. `OPENROUTER_TTS_VOICE` optionally selects a
Fish voice ID; unset uses its default voice. `DISABLE_VOICE=true` disables both directions, and an empty TTS or
STT model disables that direction. Haiku chooses optional `[voice: spoken line]` directives; a text setup can precede
a different spoken punchline. Native voice messages are Ogg/Opus with duration/waveform metadata, not MP3 downloads.
Only the first message in a multi-message reply gets a reply header. The bot needs Discord's Send Voice Messages permission.

Native incoming voice messages are limited to 8 MiB and two minutes. A PostgreSQL `voice_transcript` row is claimed
before any speech API call, so duplicate events and restarts reuse saved results, even if the selected model changes.
Failed or interrupted claims do not automatically retry and rebill; they remain available for manual review. Generated
speech text is saved directly without STT. Outgoing speech is limited to 600 characters and 60 seconds. Clips use FFmpeg's
single-threaded conversion; no local speech model is required. The opt-in `live_haiku_voice_choices` and
`live_selected_speech_models_round_trip_native_audio` tests exercise the configured APIs on synthetic content without
posting messages to Discord.

```sh
nix run                # or: docker compose up
```

`ffmpeg` and `qalc` need to be on the `PATH`; the Nix package and Docker image include them.

Reply diagnostics use `ai_timing` records keyed by `msg_id`: responder work, memory database access and shortlist, recall, profile/media preparation, stream headers, first content delta, first Discord post, and completion. `latency_ms` measures that stage; `source_age_ms` measures time since the source message was created (so edits of old messages have an older source age). Parallel stages overlap. `decider_timing` separates queue wait from service request time.

## Unprompted posts

`DECIDER_REPLY_MODE=live` enables spontaneous replies, Community Notes and argument settlements; the default is
`off`. Ordinary unanswered guild messages are checked. Direct pings, replies already being handled and DMs do not
also trigger an unprompted Note or ruling.

Community Notes require a confidently false, checkable factual claim (`DECIDER_WRONG_CLAIM_THRESHOLD=0.9`).
Rulings require a sustained disagreement between people (`DECIDER_ARGUMENT_THRESHOLD=0.85`); the drafting model
chooses a factual ruling, an opinion poll, or no post. A separate poll check (`DECIDER_POLL_THRESHOLD=0.85`) also
recognizes open preferences and group decisions, such as "miku or teto?" or "minecraft or terraria tonight?",
without requiring an argument. Drafting can still decline jokes, settled topics or unsuitable questions.

Global defaults can be overridden per channel. Cooldowns are tracked separately for each channel and post kind:
`DECIDER_NOTE_COOLDOWN_SECS=1800` and `DECIDER_RULING_COOLDOWN_SECS=3600` (rulings and polls share a cooldown).
Drafting reserves the slot against concurrent attempts; declined, failed or cancelled drafts release it. Successful
posts start the full cooldown at delivery. Logs distinguish selection (`kind="check"`, `interjection=Some(...)`)
from delivery (`kind="note" posted=true/false`, `kind="ruling"` or `kind="poll"` with `action="ruling"/"poll"/"none"`)
and failures. A declined interjection falls back to an ordinary reply only when the original reply scores warrant
one. A fresh source check before delivery prevents drafts from answering an edited or deleted message.

Set `DECIDER_CHANNEL_OVERRIDES` to a JSON object keyed by channel ID. Missing fields inherit the global defaults;
unknown fields, invalid channel IDs and thresholds outside 0–1 fail startup. For example:

```sh
DECIDER_CHANNEL_OVERRIDES='{"748957504666599507":{"poll_threshold":0.8,"note_cooldown_secs":1800,"ruling_cooldown_secs":3600}}'
```

Supported fields are `wrong_claim_threshold`, `argument_threshold`, `poll_threshold`, `note_cooldown_secs` and
`ruling_cooldown_secs`. Rulings and all polls share the latter cooldown, preventing consecutive judge/vote posts.
The default `DECIDER_CHECK_TIMEOUT_MS` is 15000, including queue time and all check questions.

Reaction GIFs are chosen during normal replies. Scheduled follow-ups only exist after a reply emits a hidden
`[predict <wait>: <claim>]` directive; they are deliberately rare. An empty prediction queue means nothing has been
scheduled, rather than that the follow-up worker is disabled.

## Tech

* [twilight](https://github.com/twilight-rs/twilight) for Discord, PostgreSQL for storage
* OpenRouter for chat models and memory embeddings (`OPENROUTER_EMBEDDING_MODEL`), and a separate decision model
  for memory recall and reply gating

## License

> This project is licensed under APACHE-2
