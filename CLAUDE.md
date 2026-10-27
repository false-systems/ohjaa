<!-- toimija:adopt:v1:begin -->
Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
<!-- toimija:adopt:v1:end -->

@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl. Read `.toimija/current.md` first, then
declare why you are here: `toimija intent "<why>" [--scope <path>]`.

This repo is one crate, and not a small one: `main.rs` plus about twenty
modules, no lib, and an integration test suite of comparable size, plus the
vendored `faija-knowledge`. Read the modules a change touches before
proposing structure for it.

An entry's gate is derived from each commit's own `sykli.json`
(`src/derived.rs`; the README's "The gate of a commit"), and a commit with no
contract of its own by what `sykli init` writes for its tree, derived again
each time it is judged. Never bring back a copy of a gate: a copy drifts.
What ohjaa reads of a clone's commits goes through its one `git cat-file --batch`
(`Sykli::ask_objects`); a git process more on a hot path needs its budget in
`tests/config_home.rs`.

Only a person accepts, rejects, retires or confirms Knowledge. Do not add
messaging, dispatch, presence, caching, or a dependency unless a human
explicitly moves it into scope and updates `.toimija/repo.yaml` and `AGENTS.md`
together.

Run the validation commands in AGENTS.md before claiming completion.
