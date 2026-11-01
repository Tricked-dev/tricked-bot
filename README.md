# Tricked-Bot

A simple discord bot made for my [discord](https://discord.gg/mY8zTARu4g)

## Features

* The Trickster: an AI regular that replies when pinged (and, optionally, when it has something to add), with
  per-user memories and image understanding
* XP and levels, plus surprise math and colour quizzes
* Slash commands: `/level`, `/qalc`, `/translate` and currency conversion (`/usd`, `/euro`, `/yen`, `/pln`, `/cad`)
* Responders, `t!ratewaifu`, a rotating channel topic and a daily profile picture
* A small web UI for editing users and memories (`WEB_PORT`)

## Running

Configuration comes from flags or environment variables (a `.env` file is loaded); see `tricked-bot --help`.
`TOKEN`, `DISCORD` (the guild to register slash commands in) and `DATABASE_URL` are the essentials.

```sh
nix run                # or: docker compose up
```

`ffmpeg` and `qalc` need to be on the `PATH`; the Nix package and Docker image include them.

## Tech

* [twilight](https://github.com/twilight-rs/twilight) for Discord, PostgreSQL for storage
* OpenRouter for chat models and memory embeddings (`OPENROUTER_EMBEDDING_MODEL`), and a separate decision model
  for memory recall and reply gating

## License

> This project is licensed under APACHE-2
