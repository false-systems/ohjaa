<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl. Read `.toimija/current.md` first, then
declare why you are here: `toimija intent "<why>" [--scope <path>]`.

This repo is one crate, but not a small one: `main.rs` and twenty-one modules,
no lib, about 25,000 lines of Rust in `src/` (the largest, `lifecycle.rs`,
`main.rs` and `sessions.rs`, over 3,000 each), with about 13,600 more in
`tests/`, plus the vendored `faija-knowledge`. Read the modules a change
touches before proposing structure for it.

Only a person accepts, rejects, retires or confirms Knowledge. Do not add
messaging, dispatch, presence, caching, or a dependency unless a human
explicitly moves it into scope and updates `.toimija/repo.yaml` and `AGENTS.md`
together.

Run the validation commands in AGENTS.md before claiming completion.
