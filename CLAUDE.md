Read `.toimija/current.md` before reading anything else in this repo; it is your workset contract. Under a toimija session your launch prompt names your live packet — that file supersedes this one.
@AGENTS.md

## Claude Code notes

Do not start with a broad file crawl. Read `.toimija/current.md` first, then
declare why you are here: `toimija intent "<why>" [--scope <path>]`.

This repo is one small crate — nine modules, no lib, plus the vendored
`faija-knowledge`. Read them before proposing structure for it.

Only a person accepts, rejects, retires or confirms Knowledge. Do not add
messaging, dispatch, presence, caching, or a dependency unless a human
explicitly moves it into scope and updates `.toimija/repo.yaml` and `AGENTS.md`
together.

Run the validation commands in AGENTS.md before claiming completion.
