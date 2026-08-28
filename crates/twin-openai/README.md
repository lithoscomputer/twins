# twin-openai

Async Rust fake OpenAI-compatible server for local black-box testing.

## Endpoints

- `GET /healthz`
- `POST /v1/responses`
- `POST /v1/chat/completions`
- `POST /__admin/scenarios`
- `POST /__admin/reset`
- `GET /__admin/requests`

`/v1/*` routes require a non-empty bearer token. Scenarios, request logs, and deterministic response IDs are scoped by bearer token so concurrent test clients can share one server safely.

`/__admin/*` routes are unauthenticated by default, but an optional bearer token selects the same namespace as `/v1/*`. Admin requests with a malformed or empty `Authorization` header are rejected.

## Run locally

```bash
cargo run -p twin-openai
```

The server binds to `127.0.0.1:3000` by default.

Set `TWIN_OPENAI_SCENARIOS_PATH` to a scenario JSON file to load a startup
template. Each bearer-token namespace receives its own copy on first use.
Resetting a namespace restores that template, clears its request log, and
restarts its deterministic response counter. When a startup template is
configured, unmatched requests fail with `scenario_not_found` by default. Set
`TWIN_OPENAI_ALLOW_UNMATCHED=true` to use deterministic fallback responses for
unmatched requests in fixture mode. Without a startup template, deterministic
fallback remains the default.

Set `TWIN_OPENAI_REQUEST_LOG_PATH` to stream normalized request records to a
JSONL file. The server creates or truncates the file at startup and flushes
each record immediately. Each line uses the same shape as an item in the
`requests` array from `GET /__admin/requests` and never includes bearer tokens.
When a scripted scenario with a `scenario_id` matches, the request record
includes that ID. The in-memory admin request log remains available when JSONL
output is enabled.

Available release binaries are attached to tags named
`twin-openai-v<version>` in GitHub Releases.

The admin and debug endpoints control scenarios and expose request data. Keep
them on a trusted local interface. Do not expose them to the public internet.

## Admin scripting

Load deterministic one-shot scenarios:

```bash
curl -X POST http://127.0.0.1:3000/__admin/scenarios \
  -H 'Authorization: Bearer suite-a' \
  -H 'content-type: application/json' \
  -d '{
    "scenarios": [
      {
        "scenario_id": "first-response",
        "matcher": { "endpoint": "responses", "model": "gpt-test", "stream": false },
        "script": { "kind": "success", "response_text": "scripted reply" }
      }
    ]
  }'
```

`scenario_id` is optional. Non-empty IDs must be unique within the active
scenario queue for a namespace.

Inspect normalized request logs:

```bash
curl http://127.0.0.1:3000/__admin/requests \
  -H 'Authorization: Bearer suite-a'
```

Reset scenarios, logs, and deterministic counters:

```bash
curl -X POST http://127.0.0.1:3000/__admin/reset \
  -H 'Authorization: Bearer suite-a'
```

## Behavior summary

- Non-stream and stream success paths are driven from the same canonical response plan.
- `/v1/responses` and `/v1/chat/completions` share the same deterministic fallback behavior.
- Structured output supports `json_object` and a documented `json_schema` subset.
- Scripted failures support OpenAI-shaped application errors, delays, hangs, partial streams, and malformed SSE.

## Recorded Contract Fixtures

The twin's contract with the live OpenAI API is recorded on disk and
replayed offline:

- `tests/common/cases.rs` defines one request per covered surface. Every
  prompt embeds a `[case:<id>]` marker.
- `tests/snapshots/` holds canonical snapshots of each exchange, projected
  onto the compatibility matrix: volatile values and generated text are
  redacted, generated JSON is parsed, and stream chunks collapse into
  milestone sequences. Each case also has a `__live_extras` snapshot listing
  observed fields outside the contract, so drift there stays visible.
- `fixtures/scenarios.json` holds scenarios derived from the captures, with
  the genuinely captured content. The replay suite (`tests/replay_contract.rs`)
  loads it in strict fixture mode and asserts the same snapshots through the
  twin on every normal `cargo test` run, with no network.

Re-record against the live API with:

```bash
OPENAI_API_KEY=... mise run record
```

`mise run record` is the only blessed write path for these snapshots. Do not
`cargo insta accept` a replay failure: that would overwrite the recorded live
truth with twin output. A replay failure after re-recording means the twin no
longer reproduces the live contract, and the engine needs work.

The nightly `Nightly OpenAI drift` workflow re-records on a schedule. When
the canonical shape changed, it opens a PR with the updated snapshots and
fixtures and reports whether the replay suite still passes (passing = content
churn, failing = real drift). When only captured content changed, it discards
the churn and exits quietly.

Record and replay must use the same model. `TWIN_OPENAI_LIVE_MODEL` overrides
the default for both suites.

## Proxy-Record Mode

Set `TWIN_OPENAI_MODE=proxy-record` to turn the server into a recording
proxy for an application's E2E suite:

```bash
TWIN_OPENAI_MODE=proxy-record \
OPENAI_API_KEY=sk-... \
TWIN_OPENAI_RECORDING_PATH=recordings/scenarios.json \
cargo run -p twin-openai
```

- Point the app's OpenAI client at the twin, with a fake test-scoped bearer
  token per E2E test (for example `e2e-checkout-flow`). The bearer names the
  recording namespace. The twin replaces it with `OPENAI_API_KEY` when
  forwarding, so no secret ever lands in a recording.
- `/v1/responses` and `/v1/chat/completions` are forwarded to
  `TWIN_OPENAI_UPSTREAM_URL` (default `https://api.openai.com`) and streamed
  back verbatim.
- Every successful exchange is derived into a scripted scenario appended to
  the recording file, with ordered ids like `e2e-checkout-flow/0001` and a
  loose matcher (endpoint + stream). The file is truncated at startup and
  rewritten atomically after each exchange, so an interrupted run keeps what
  it captured.
- Failed upstream responses and underivable exchanges pass through without
  being recorded. Admin and debug routes are not mounted in this mode.

To replay, start the twin normally with
`TWIN_OPENAI_SCENARIOS_PATH=recordings/scenarios.json` and run the same E2E
suite unchanged. Scenarios with a `namespace` seed only that bearer's
namespace and replay in recorded order. Strict fixture mode fails loudly on
an exhausted queue or an unrecorded bearer. Hand-scripted failure scenarios
still compose on top through `POST /__admin/scenarios`.

Replay renders content through the twin's canonical engine: the app sees the
recorded content and event sequence, not the original chunk boundaries or
timing.

## Optional Live OpenAI Smoke Suite

Run the ignored live drift detector only when you explicitly want to compare `twin-openai` against the real OpenAI API:

```bash
OPENAI_API_KEY=... cargo test --test live_openai_contract -- --ignored --nocapture
```

Optional environment variables:

- `TWIN_OPENAI_LIVE_MODEL` defaults to `gpt-5-nano-2025-08-07`
- `TWIN_OPENAI_LIVE_BASE_URL` defaults to `https://api.openai.com`
- `OPENAI_ORGANIZATION` and `OPENAI_PROJECT` are forwarded when present

This suite is not part of normal CI. It is intentionally a drift detector for request/response shape and SSE sequencing, so opt-in failures can represent real compatibility gaps rather than a broken local test harness.

If the supplied OpenAI credentials lack required endpoint scopes or quota, the ignored test will skip the blocked live surface instead of reporting protocol drift.

Current live coverage includes `responses` and `chat.completions` text, streaming, structured output, function tools, `tool_choice: "none"` behavior, image-input acceptance, and both non-stream and streamed `responses` continuation turns.

See [docs/compatibility-matrix.md](docs/compatibility-matrix.md) for the
supported field matrix and explicit exclusions.
