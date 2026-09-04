# Compatibility matrix

The target is the Anthropic Messages protocol used by `lithos-llm`, with the developer tools of `twin-openai`.

| Surface | Support |
| --- | --- |
| Generation | `POST /v1/messages`, JSON and SSE |
| Counting | `POST /v1/messages/count_tokens`; approximate fallback and exact scripts |
| Models | `GET /v1/models`; deterministic one-page listing |
| Credentials | `x-api-key`; bearer alias; namespace isolation; optional auth |
| Protocol headers | `anthropic-version: 2023-06-01`; beta headers forwarded in proxy mode |
| Text | String and text-block messages; top-level system and system turns |
| Tools | Tool definitions, auto/none/any/named choice, tool_use and tool_result history |
| Thinking | Enabled/adaptive/disabled controls; thinking and signature deltas; redacted blocks |
| Images/documents | Base64 and URL sources; text/content document sources |
| Structured output | `output_config.format.type: json_schema` with object roots, nested objects, primitives, enum and const |
| Metadata/cache controls | Accepted; metadata matching; scripted disjoint cache usage counters |
| Native extensions | Unknown top-level request fields and native content block types accepted; native content scripts and transcript replay preserve provider data |
| Stop state | end_turn, max_tokens, stop_sequence, tool_use, pause_turn, refusal, model_context_window_exceeded |
| Streaming | message_start; content block start/deltas/stop; message_delta; message_stop; no OpenAI DONE marker |
| Error scripting | Native HTTP errors, Retry-After, custom headers, native stream errors through raw/transcript scripts |
| Transport scripting | Header/event delays, hangs, partial streams, malformed SSE, exact bytes, body failures |
| Admin/debug | Scenario queue, reset, request logs, JSONL, HTML debug page, JSON state |
| Fixtures | Startup templates, strict matching, repeat/sticky, namespace scoping, reset restoration |
| Recording | Semantic and transcript modes; credential replacement; append; atomic file replacement |
| Testing | Offline wire corpus and replay; opt-in live captures and scheduled drift workflow |
| Distribution | Rust library/binary, Dockerfile, binary/image release workflow and attestations |

The stream sequence follows [Anthropic's streaming reference](https://platform.claude.com/docs/en/build-with-claude/streaming). Request and response shapes also follow the [Messages API reference](https://platform.claude.com/docs/en/api/typescript/messages). The checked-in wire corpus comes from `../lithos-llm/tests/it/wire/snapshots/it__wire__anthropic__*.snap`. It contains request examples, not captured live responses.

## Limits

This is a protocol twin, not a language model. It does not generate real answers, execute tools, tokenize with a model tokenizer, simulate cache hits, enforce model-specific capabilities, or infer tool choice from a prompt. Sampling controls and most provider options are accepted without simulating their effect. Scripted headers and usage let tests cover those behaviors explicitly.

Structured schema generation intentionally matches the small subset supported by `twin-openai`; arrays, unions, references, and arbitrary JSON Schema constraints are not implemented. The `lithos-llm` free-form JSON-object option is a system instruction, so use a structured_output script when the test needs a JSON object.

Image/document source shapes are validated. File contents, base64 decoding, remote URLs, and thinking signatures are not authenticated. Native content scripts can reproduce opaque server-tool blocks, but the twin does not execute the server tools.

Batch requests, Files, Skills, Agents, Sessions, model retrieval by ID, and model pagination are outside this crate's endpoint surface. Generation accepts any non-empty model string. Utility endpoints use local fallbacks during replay unless explicitly scripted.

Transcript replay preserves original JSON text and SSE event data, including provider extensions. It normalizes SSE framing and does not preserve comment/id/retry fields, original network chunks, or timing. Semantic recording preserves native content and usage but rejects unknown stream deltas, incomplete tool JSON, and streams that never reach message_stop. Use transcript recording for those exchanges.

The initial generation contract fixtures are synthetic. Live capture is opt-in and requires credentials. Release workflows are provided; publication requires pushing a release tag separately.
