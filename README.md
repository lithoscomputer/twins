# Twins

Deterministic protocol twins for black-box integration tests.

This repository currently contains [`twin-openai`](crates/twin-openai), an
OpenAI-compatible HTTP server for local testing.

## Run twin-openai

Download a binary from the GitHub Releases page, run the Docker image, or
build it locally:

```bash
docker run --rm -p 3000:3000 ghcr.io/lithoscomputer/twin-openai:latest
```

```bash
cargo run -p twin-openai
```

Outside Docker, the server listens on `127.0.0.1:3000` by default. See the
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

Linux binaries target GLIBC 2.35, the baseline provided by Ubuntu 22.04. The
release workflow rejects binaries that require a newer GLIBC version.

Release archives include GitHub build-provenance attestations. Verify a
downloaded archive with:

```bash
gh attestation verify twin-openai-linux-x86_64.tar.gz --repo lithoscomputer/twins
```

The multi-platform Docker image supports Linux x86_64 and Linux ARM64. Each
version uses the matching binary from the GitHub release build. Verify its
build-provenance attestation with:

```bash
gh attestation verify oci://ghcr.io/lithoscomputer/twin-openai:latest --repo lithoscomputer/twins
```

## Development

```bash
cargo fmt --check --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The live OpenAI comparison suite is ignored during normal test runs. Its
credentials and command are documented in the `twin-openai` README.
