# Contributing

taskloom is a typed LLM task executor for Rust with an optional agent runtime.
Keep task-specific business logic outside the execution, gateway and provider
layers. Keep runtime capabilities above the single-task core; existing users
must not need to enable runtime dependencies.

Use Rust 1.86 or newer. Before submitting a pull request, run:

```text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo doc --locked --no-deps --all-features
```

CI checks Linux, Windows, the minimum supported Rust version, individual
features, offline examples and crate packaging. Tests should use fake
providers or local HTTP servers and must not require API credentials.
Add regressions for changes to retries, deadlines, cancellation, accounting,
cache identity and recording behavior. Keep new public APIs documented.
Runtime changes also need coverage of dispatch permissions, tool correlation,
limits, interrupted side effects and model-spend reservations. See
[the development roadmap](docs/agent-roadmap.md) for phase acceptance criteria.

Never commit API keys, local environment files, recordings or cached customer
data. Report ordinary bugs through GitHub issues with a minimal reproduction.
For sensitive issues, use private vulnerability reporting from the repository's
Security tab rather than putting credentials or exploit details in an issue.

Contributions are dual licensed under MIT or Apache-2.0, following the licenses
included in this repository.
