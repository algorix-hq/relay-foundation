# Dependency

Consumers use git dependencies pinned to a commit with `rev`. There are no release tags or crates.io releases. The crate version is always `0.0.0` and carries no release meaning.

Replace `<commit>` with a full 40-character commit SHA.

```toml
[dependencies]
relay-protocol = { git = "https://github.com/algorix-hq/relay-foundation", rev = "<commit>" }
relay-bridge   = { git = "https://github.com/algorix-hq/relay-foundation", rev = "<commit>" }
relay-adapter  = { git = "https://github.com/algorix-hq/relay-foundation", rev = "<commit>" }
relay-client   = { git = "https://github.com/algorix-hq/relay-foundation", rev = "<commit>", features = ["reqwest"] }
relay-catalog  = { git = "https://github.com/algorix-hq/relay-foundation", rev = "<commit>" }
serde_json = "1"
```

- Use the same `rev` for all `relay-*` crates. Mixing revs gives two copies of `relay-protocol` and type mismatches such as "expected `Request`, found `Request`".
- Do not use `branch` or `tag`. To find the latest commit: `git ls-remote https://github.com/algorix-hq/relay-foundation refs/heads/main`.
- Depend only on the crates you use. A consumer that only converts between formats needs `relay-protocol` and `relay-bridge`. Anything that talks to a provider also needs `relay-adapter`.
- `relay-catalog` is optional, only needed when using catalog types.
- Default builds bring no HTTP client or runtime. `relay-client`'s opt-in `reqwest` feature provides `ReqwestTransport`; omit `features = ["reqwest"]` for an injected transport only. The application still supplies its runtime. See `client.md`.

## Upgrading

1. Read the commit log between the old and new rev (`git log <old>..<new>`) and look for `!` and `BREAKING CHANGE:` footers.
2. Change the rev in all `relay-*` entries together.
3. Run `cargo update -p relay-protocol -p relay-bridge -p relay-adapter` (include `-p relay-client` and `-p relay-catalog` if used), then `cargo check`.
4. Re-check the references against the new rev's source if `cargo check` reports differences.
