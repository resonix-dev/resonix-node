# Contributing to Resonix

Thanks for your interest in contributing!

## How to contribute
- Discuss major changes first by opening an issue.
- For small fixes and docs, feel free to submit a PR directly.

## Development setup
- Install Rust (stable) and `cargo`.
- Optional: `yt-dlp` in PATH if you want to test resolver behavior locally.
- Build: `cargo build` | Format: `cargo fmt`

## Coding guidelines
- Rust 2021 edition.
- Keep public APIs minimal and well-documented.
- Add comments where behavior is non-obvious.
- Prefer small, focused PRs.

## Rust style
- Use `rustfmt` with the repo’s `rustfmt.toml`.
- Run `cargo fmt` before committing.

## Testing
- Include minimal repro steps or smoke tests where practical.
- For audio paths, include logs and environment details.

## Commit messages
- Use clear, imperative subject lines; include context in the body when needed.

## Pull request checklist
- [ ] `cargo fmt` passes
- [ ] Build succeeds (`cargo build`)
- [ ] PR description explains the change and impact
- [ ] Docs updated when behavior changes

## License
- By contributing, you agree your contributions are licensed under the BSD-3-Clause license.
