# Dependency

Relay is not published to crates.io. Consumers use git dependencies pinned to a tag. A tag is a release.

```toml
[dependencies]
relay-protocol = { git = "https://github.com/algorix-hq/relay-foundation", tag = "v1.0.0-alpha.1" }
relay-bridge   = { git = "https://github.com/algorix-hq/relay-foundation", tag = "v1.0.0-alpha.1" }
relay-adapter  = { git = "https://github.com/algorix-hq/relay-foundation", tag = "v1.0.0-alpha.1" }
serde_json = "1"
```

- Use the same tag for all three crates. Mixing tags gives two copies of `relay-protocol` and type mismatches such as "expected `Request`, found `Request`".
- `v0.x` tags belong to the previous implementation with a different API. Do not use them for new code. `v1.0.0-alpha.1` is the first tag of this implementation.
- Do not use `branch = "main"` or an untagged `rev`. The API changes between tags, and breaking changes are marked with `!` in commit messages.
- To list available tags: `git ls-remote --tags https://github.com/algorix-hq/relay-foundation`.
- Depend only on the crates you use. A consumer that only converts between formats needs `relay-protocol` and `relay-bridge`. Anything that talks to a provider also needs `relay-adapter`.
- Relay brings no HTTP client and no runtime. Add your own, for example `reqwest` and `tokio`.

## Upgrading

1. Read the commit log between the old and new tag and look for `!` and `BREAKING CHANGE:` footers.
2. Change the tag in all three entries together.
3. Run `cargo update -p relay-protocol -p relay-bridge -p relay-adapter`, then `cargo check`.
4. Compare against the new tag's `EXAMPLES.md`.
