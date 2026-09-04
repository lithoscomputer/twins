# twin-anthropic

An async Rust Anthropic-compatible server for deterministic local integration tests. It has the scenario, logging, debug, failure injection, and recording tools of `twin-openai`.

## Start

```bash
cargo run -p twin-anthropic
```

The default address is `127.0.0.1:3001`, so both twins can run together.

```bash
curl http://127.0.0.1:3001/v1/messages \
  -H 'x-api-key: suite-a' \
  -H 'anthropic-version: 2023-06-01' \
  -H 'content-type: application/json' \
  -d '{"model":"claude-test","max_tokens":128,"messages":[{"role":"user","content":"hello"}]}'
```

Generation accepts any non-empty model ID. The fallback replies `deterministic: hello`, with IDs such as `msg_000001`. Each API key has its own scenarios, IDs, and request log. Use fake test keys.

Library entry points are `twin_anthropic::build_app()` and `build_app_with_config(Config)`. Both return an Axum router. `Config::from_lookup` supports explicit configuration without changing process environment variables.

## Use with lithos-llm

The server supports the Messages and token-counting requests produced by `lithos-llm`. With its CLI available:

```bash
ANTHROPIC_API_KEY=suite-a lllm \
  --catalog crates/twin-anthropic/examples/lithos-llm.toml \
  -m anthropic/claude-test 'Hello'
```

The example catalog redirects the Anthropic provider to this twin. Add `--no-stream` to exercise JSON responses. The checked-in request corpus covers 31 wire examples from `lithos-llm`, including two deliberately incomplete passthrough requests that the twin rejects.

## Endpoints

| Endpoint | Behavior |
| --- | --- |
| `GET /healthz` | Unauthenticated health check |
| `POST /v1/messages` | JSON or Anthropic SSE generation |
| `POST /v1/messages/count_tokens` | Deterministic input-token estimate or a script |
| `GET /v1/models` | One stable `claude-test` model entry |
| `POST /__admin/scenarios` | Append scripts to the selected namespace |
| `POST /__admin/reset` | Restore startup scripts, clear logs, and reset IDs |
| `GET /__admin/requests` | Read normalized request records |
| `GET /__debug` | Debug UI with automatic refresh |
| `GET /__debug/state.json` | Inspect all active namespaces |

`/v1/*` requires a non-empty `x-api-key` and `anthropic-version: 2023-06-01`. A bearer token also selects an API-key namespace. Conflicting credentials are rejected. `TWIN_ANTHROPIC_REQUIRE_AUTH=false` allows requests without a key to use the global namespace; the version header is still required.

Admin routes require no credentials. Supply the test's `x-api-key` or bearer token to select its namespace. Without credentials they use the global namespace. Malformed credentials are rejected. Debug routes expose all namespaces. Keep admin and debug access on a trusted local interface, or disable them with `TWIN_ANTHROPIC_ENABLE_ADMIN=false`.

Token counting uses one token per four bytes of compact request JSON, rounded up. It is an estimate, not a model tokenizer. Unlike generation, counting rejects `max_tokens`, `stream`, and other generation-only fields. Model discovery and unscripted counts do not consume generation scenarios, IDs, or logs.

## Scenarios

```bash
curl http://127.0.0.1:3001/__admin/scenarios \
  -H 'x-api-key: suite-a' -H 'content-type: application/json' \
  -d '{"scenarios":[{
    "scenario_id":"greeting",
    "matcher":{"endpoint":"messages","input_contains":"hello","stream":false},
    "script":{"kind":"success","response_text":"Hello from the fixture."}
  }]}'
```

Scripts answer the first matching request in queue order. A scenario is one-shot by default. `"repeat": 3` answers three matches; `"sticky": true` answers every match until reset. `scenario_id` is optional, but supplied IDs must be non-empty and unique in the active queue. Invalid scenario batches are rejected without changing the queue.

Matchers support `endpoint` (`messages` or `messages.count_tokens`), `model`, `stream`, `metadata`, `input_contains`, `instructions_contains`, and `request_hash`. Input matching includes user text and returned tool results. Instruction matching includes the top-level `system` field and system turns. Hashes ignore whitespace and recursively ignore object key order; array order remains significant.

Success scripts support:

- `response_text`, `reasoning` (an array of strings), and `structured_output` (a JSON value).
- `tool_calls`: objects with `name`, `arguments` (or `input`), optional `id`, and optional `raw_arguments` for partial JSON streaming tests.
- `content`: an entire native content-block array. This overrides generated content and preserves thinking signatures, redacted thinking, tool IDs, and server-tool blocks.
- `usage`: input/output tokens, cache creation/read counters, and extra provider counters.
- `stop_reason`, `stop_sequence`, and `stop_details`. `finish_reason: length` is an alias for `max_tokens`.
- `input_tokens` for an exact token-count response.
- `headers`, `delay_before_headers_ms`, `inter_event_delay_ms`, `close_after_chunks`, and `malformed_sse`.

Example tool response:

```json
{
  "matcher": {"endpoint":"messages"},
  "script": {
    "kind":"success",
    "tool_calls":[{"id":"toolu_weather","name":"weather","arguments":{"city":"Paris"}}],
    "usage":{"input_tokens":9,"output_tokens":4,"cache_read_input_tokens":100}
  }
}
```

A tool-only script emits no fabricated text. Forced tool choice (`any` or `tool`) requires a matching scripted tool response. `none` rejects a script that tries to return a tool call. The fallback does not infer tool calls from a prompt.

`output_config.format` supports a documented JSON-schema subset. `lithos-llm` implements its free-form JSON-object option with a system instruction; use `structured_output` to script that result. The twin does not interpret arbitrary instructions as generation rules.

## Failure and transport scripts

```json
{
  "matcher":{"endpoint":"messages"},
  "repeat":3,
  "script":{
    "kind":"error",
    "status":529,
    "error_type":"overloaded_error",
    "message":"Try again later",
    "retry_after":"2"
  }
}
```

Errors have the native `{"type":"error","error":{"type":...,"message":...}}` envelope. Optional `code` identifies a test-specific failure. Error scripts support headers and a delay before headers. `{"kind":"hang"}` never sends a response.

Raw scripts return exact chunks, arbitrary bytes, and body failures:

```json
{
  "matcher":{"endpoint":"messages"},
  "script":{
    "kind":"raw",
    "status":200,
    "content_type":"application/octet-stream",
    "headers":{"x-test":"raw"},
    "delay_before_headers_ms":10,
    "chunks":[
      {"kind":"text","text":"prefix"},
      {"kind":"bytes","bytes":[0,255,254],"delay_ms":10},
      {"kind":"error","message":"connection reset","delay_ms":20}
    ]
  }
}
```

A body error ends the stream. Later chunks are not sent. The explicit `content_type` overrides a `content-type` header. Transcript scripts can also inject native SSE `error` events. Partial streams omit terminal events; malformed SSE adds an invalid frame.

## Fixtures and logs

Set `TWIN_ANTHROPIC_SCENARIOS_PATH` to a JSON envelope containing `scenarios`. Each namespace receives its own template copy on first use. A scenario with `namespace` seeds only that API key. A reset restores the template. Fixture mode rejects unmatched generation calls with `scenario_not_found`; set `TWIN_ANTHROPIC_ALLOW_UNMATCHED=true` to allow fallback responses.

Set `TWIN_ANTHROPIC_REQUEST_LOG_PATH` to a JSONL file. The file is created or truncated at startup, then flushed after each record. It has the same records as `/__admin/requests`, including matched scenario IDs and normalized input, system instructions, and metadata. Credentials are not included. The in-memory log remains available.

## Record and replay an application

```bash
TWIN_ANTHROPIC_MODE=proxy-record \
TWIN_ANTHROPIC_UPSTREAM_API_KEY=... \
TWIN_ANTHROPIC_RECORDING_PATH=recordings/scenarios.json \
cargo run -p twin-anthropic
```

Point the application's Anthropic base URL at the twin and use a fake API key per test. The proxy replaces it with the upstream key. It forwards the version and beta headers and returns upstream status, body, request IDs, and rate-limit headers. Admin and debug routes are absent in proxy mode.

Successful generation exchanges become scenarios scoped to the client's key, with IDs such as `suite-a/0001`. Semantic recording preserves native content, tool IDs, thinking signatures, usage, and stop reasons. It renders replay through the shared response plan. Chunk boundaries and timing are not reproduced. Errors, failed streams, and underivable semantic exchanges pass through without being recorded. Model discovery and counts pass through without recording.

For exact JSON bodies and SSE event sequences, set `TWIN_ANTHROPIC_RECORD_FORMAT=transcript`. Transcript matching uses the canonical request hash, so distinct requests can replay in a different order. Original JSON text and provider fields survive unchanged. SSE field formatting and transport chunk boundaries are normalized; events remain separate. Recorded response headers are retained except hop-by-hop, content-length, and credential headers.

Recording files are replaced atomically after each complete exchange. `TWIN_ANTHROPIC_RECORDING_APPEND=true` preserves existing scenarios and continues namespace numbering. A streamed exchange is recorded only after the upstream body reaches EOF.

Replay with:

```bash
TWIN_ANTHROPIC_SCENARIOS_PATH=recordings/scenarios.json cargo run -p twin-anthropic
```

Keep the application's fake test keys unchanged. Semantic scenarios replay in recorded order within each namespace; transcript scenarios also match the request body. Admin scripts can append extra test behavior during replay.

## Contract testing and drift

```bash
cargo test --locked -p twin-anthropic
mise run replay:anthropic
```

`fixtures/contracts.json` describes 12 generation exchanges: text, tools, structured output, thinking, tool continuation, and images, each with and without streaming. `fixtures/scenarios.json` replays them offline. The initial contracts are **synthetic**, based on the protocol and `lithos-llm` wire tests. They are not represented as live captures.

Run a live smoke comparison explicitly:

```bash
ANTHROPIC_API_KEY=... cargo test --locked -p twin-anthropic \
  --test live_anthropic_contract -- --ignored --nocapture
```

To capture the live contract and regenerate semantic fixtures:

```bash
ANTHROPIC_API_KEY=... mise run record:anthropic
```

This is the write path for live contract fixtures. Do not change an expected contract to make a replay failure pass. The capture strips generated values from the contract, retains exact content in scenarios, and tracks extra top-level response fields. It also smoke-tests model discovery and token counting. A failed capture leaves the existing fixture set intact until all calls succeed.

`TWIN_ANTHROPIC_LIVE_MODEL` defaults to `claude-sonnet-4-6`; `TWIN_ANTHROPIC_LIVE_BASE_URL` defaults to `https://api.anthropic.com`. These settings apply only to the ignored live suite. No live calls run in normal CI.

The `Nightly Anthropic drift` workflow uses the repository's `ANTHROPIC_API_KEY` secret. It opens a PR when contracts change and reports whether offline replay passes. Content-only fixture churn is discarded. Configure the secret to enable captures; a missing key skips the live suite.

## Configuration

| Environment variable | Default |
| --- | --- |
| `TWIN_ANTHROPIC_BIND_ADDR` | `127.0.0.1:3001` |
| `TWIN_ANTHROPIC_REQUIRE_AUTH` | `true` |
| `TWIN_ANTHROPIC_ENABLE_ADMIN` | `true` |
| `TWIN_ANTHROPIC_SCENARIOS_PATH` | unset |
| `TWIN_ANTHROPIC_ALLOW_UNMATCHED` | `false` |
| `TWIN_ANTHROPIC_REQUEST_LOG_PATH` | unset |
| `TWIN_ANTHROPIC_MODE` | `twin`; also accepts `proxy-record` |
| `TWIN_ANTHROPIC_UPSTREAM_URL` | `https://api.anthropic.com` |
| `TWIN_ANTHROPIC_UPSTREAM_MESSAGES_PATH` | `/v1/messages` |
| `TWIN_ANTHROPIC_UPSTREAM_API_KEY` | falls back to `ANTHROPIC_API_KEY` |
| `TWIN_ANTHROPIC_RECORDING_PATH` | required in proxy mode |
| `TWIN_ANTHROPIC_RECORD_FORMAT` | `semantic`; also accepts `transcript` |
| `TWIN_ANTHROPIC_RECORDING_APPEND` | `false` |

Booleans accept `true`/`false` and `1`/`0`. Invalid configuration fails startup.

## Releases

Tags named `twin-anthropic-v<version>` build macOS ARM64, Linux x86_64, and Linux ARM64 binaries, with the same GLIBC 2.35 compatibility check and provenance attestations as `twin-openai`. The release workflow also builds and publishes `ghcr.io/lithoscomputer/twin-anthropic` using `Dockerfile.anthropic`. No release is published by adding this crate.

See [the compatibility matrix](docs/compatibility-matrix.md) for the supported protocol surface and exclusions.
