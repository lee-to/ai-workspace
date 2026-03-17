# Contributing to AI Workspace

Thank you for your interest in contributing! Here's how you can help.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/<your-username>/ai-workspace.git`
3. Create a branch: `git checkout -b my-feature`
4. Make your changes
5. Run tests: `cargo test`
6. Run lints: `cargo fmt --check && cargo clippy -- -D warnings`
7. Commit and push
8. Open a Pull Request against `main`

## Prerequisites

- Rust 1.85+ (edition 2024)
- No external dependencies required (SQLite is bundled)

## Development

```bash
# Build
cargo build

# Run tests
cargo test

# Run lints
cargo fmt --check
cargo clippy -- -D warnings

# Run locally
cargo run -- init --group mygroup
cargo run -- serve
```

## Pull Requests

- Keep PRs focused — one feature or fix per PR
- Add tests for new functionality
- Make sure all tests pass and clippy is clean
- Follow existing code style (run `cargo fmt`)

## Reporting Issues

Open an issue at [github.com/lee-to/ai-workspace/issues](https://github.com/lee-to/ai-workspace/issues) with:

- What you expected to happen
- What actually happened
- Steps to reproduce
- OS and Rust version

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
