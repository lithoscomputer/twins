# Twins

Deterministic protocol twins for black-box integration tests.

This repository currently contains [`twin-openai`](crates/twin-openai), an
OpenAI-compatible HTTP server for local testing.

## Run twin-openai

Download a binary from the GitHub Releases page, or build it locally:

```bash
cargo run -p twin-openai
```

The server listens on `127.0.0.1:3000` by default. See the
[`twin-openai` documentation](crates/twin-openai/README.md) for its endpoints,
configuration, and scenario API.

## Use the Rust library

Pin the repository revision so builds use an exact version:

```toml
[dev-dependencies]
twin-openai = { git = "https://github.com/lithoscomputer/twins", rev = "<commit-sha>" }
```

The library exports an Axum router through `build_app()` and
`build_app_with_config()`.

## Release platforms

Each release includes binaries for:

- macOS ARM64
- Linux x86_64
- Linux ARM64

## Development

```bash
cargo fmt --check --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The live OpenAI comparison suite is ignored during normal test runs. Its
credentials and command are documented in the `twin-openai` README.
