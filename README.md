# Tricked-Bot

A simple discord bot made for my [discord](https://discord.gg/mY8zTARu4g)

## Features

* The Trickster: an AI regular that replies when pinged (and, optionally, when it has something to add), with
  per-user memories and image understanding. It can attach reaction GIFs (anime reactions from nekos.best with no
  key, or any GIF search with `KLIPY_API_KEY`; `DISABLE_GIFS` turns them off) and comes back to gloat about its
  predictions once they're due
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

```sh
nix run                # or: docker compose up
```

`ffmpeg` and `qalc` need to be on the `PATH`; the Nix package and Docker image include them.

Reply diagnostics use `ai_timing` records keyed by `msg_id`: responder work, memory database access and shortlist, recall, profile/media preparation, stream headers, first content delta, first Discord post, and completion. `latency_ms` measures that stage; `source_age_ms` measures time since the source message was created (so edits of old messages have an older source age). Parallel stages overlap. `decider_timing` separates queue wait from service request time.

## Tech

* [twilight](https://github.com/twilight-rs/twilight) for Discord, PostgreSQL for storage
* OpenRouter for chat models and memory embeddings (`OPENROUTER_EMBEDDING_MODEL`), and a separate decision model
  for memory recall and reply gating

## License

> This project is licensed under APACHE-2
