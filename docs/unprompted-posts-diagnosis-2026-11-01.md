# Unprompted posts on beru, 2026-11-01

Investigated the active `tricked-bot` systemd service over SSH and its journal from midnight through roughly
16:40 Europe/Amsterdam. Runtime mode is `DECIDER_REPLY_MODE=live`; reply model is Haiku 5.5. No configuration
or service changes were made on the host.

## Observed behaviour

- 223 completed checks: wrong-claim scoring ran 73 times, argument scoring 31 times.
- Four argument checks selected a ruling; all four drafting requests returned `action="none"`.
- One wrong-claim check selected a Note; drafting returned `posted=false`.
- No Note, ruling or poll was delivered in this sample. There were no interjection delivery errors.
- Nine background checks exceeded their five-second deadline. This loses opportunities but does not explain
  the five selected drafts backing out.
- A GIF was successfully posted at 15:50 local time. GIF delivery is enabled and works.
- No `Prediction made` records appeared in today's journal. PostgreSQL had zero pending/due predictions.

Examples: the 15:53:28 local ruling selection (`1566465186817253465`) returned `none` a second later; its
source message was a bare GIF link. The 15:53:45 Note selection (`1566465258392920137`) backed out at
15:53:48. The other ruling candidates were banter rather than a sustained human disagreement. The initial
classifier can misread these; retaining the drafting model's option to decline is appropriate.

## Confirmed bug and local fix

`src/decider/check.rs` claimed the full channel cooldown before requesting a draft. The outcome never updated
that claim. Even `none`, `note:null`, API failures or cancellation could therefore block new Notes for 30 minutes
and rulings/polls for an hour without posting anything.

The local fix treats the claim as a reservation: a confirmed post starts the cooldown at delivery; a declined,
failed or cancelled draft releases its matching reservation. An older completion cannot remove a newer claim.

The mock-model regression test `declined_or_failed_drafts_do_not_spend_the_channel_cooldown` failed before the
fix with `Note must remain eligible after no post; model_available=true` and passed after it. Additional tests
cover successful delivery retaining the cooldown, abandonment, and stale claim completion.

Validation: 117 tests passed, 8 opt-in tests ignored; strict Clippy, formatting and diff checks passed. The
configuration-default test requires unsetting the local `DECIDER_REPLY_MODE` override inherited from the dev
shell. The fix has not been committed or deployed.

## Behaviour identified during diagnosis (subsequently addressed below)

Polls currently share the sustained-argument gate with rulings. They are not general spontaneous polls and do
not automatically respond to preference questions, explicit pings or conversations with the bot. To make polls
useful outside arguments, add a separate decision question for a clear choice the room can vote on, with its own
channel cooldown. Keep Notes conservative; lowering numerical thresholds would not help this sample much:
71 of 73 wrong-claim scores were zero, and argument scores were only zero or one.

Cooldown state is per channel; confidence thresholds are global configuration values. True per-channel threshold
overrides are not implemented. Predictions require the reply model to emit a hidden prediction directive before
there is any scheduled work. Their rarity in the prompt and empty queue explain the absence of follow-ups in
this sample; this does not independently prove end-to-end live follow-up delivery.

## Approved follow-up implementation

After the user approved fixing and deploying the features, the implementation added a separate poll question and
drafting prompt for open preferences/group decisions, validated `DECIDER_CHANNEL_OVERRIDES`, a 15-second check
deadline, and a normal-reply fallback after declined interjections when the original reply scores qualify.
Rulings and polls share one channel cooldown to avoid consecutive judge/vote posts. Notes retain their factual
safeguards. Pending drafts cannot expire even with zero cooldown, and drafted posts check for edited/deleted
sources again immediately before sending.

Validation now passes 121 tests (8 opt-in tests ignored), strict Clippy and formatting. A mock end-to-end test
routes a lone ordinary preference question through the actual check and model/delivery pipeline into a poll.
Six synthetic cases against beru's configured Decider and Haiku all passed: two open-choice polls, a factual
question that generated no poll, a false fact that generated a Note, an opinion that generated no Note, and a
factual disagreement that generated a ruling. These evaluations posted nothing to Discord. Read-only permission
checks confirmed the bot can send messages, embed links and send polls in both active investigated channels.
