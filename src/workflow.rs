//! The workflow from outside the repositories: `ohjaa doctor`, `ohjaa adopt`,
//! `ohjaa gate` and `ohjaa shell-init` (`ohjaa apply` is in `apply`).
//!
//! `doctor` reads what `apply` writes and, clone by clone, whether git really
//! runs ohjaa's hooks where an entry says it should. `adopt` writes a
//! repository's entry into the home and regenerates the home's git config;
//! it never touches git's global config, and never writes into a repository
//! (it takes no lock there either: no `git status`). `gate` runs the gate a
//! push will judge.
//!
//! Each reads its facts once: git's global config in one process, a clone's
//! config in one, its common directory from the layout.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::apply::{
    ignore_with_block, launch_target, read_or_empty, sources_old_launcher, IGNORED, IGNORE_BEGIN,
};
use crate::gate::{self, Listing};
use crate::home::{self, RepoId};
use crate::hook::{common_dir_in, dispatcher_target, facts_for_sykli, quote_path, HOOK_NAMES};
use crate::process;

/// The change was refused.
const REFUSED: i32 = 3;
/// `doctor`: something differs from what `apply` would make it.
const DRIFT: i32 = 4;

/// The launcher, printed by `ohjaa shell-init zsh`. Its rules are the
/// person's own: see the comments in the file.
const LAUNCH_ZSH: &str = include_str!("launch.zsh");

pub fn shell_init(shell: &str) -> Result<i32> {
    if shell != "zsh" {
        eprintln!("ohjaa shell-init: only zsh is supported, not {shell}");
        return Ok(2);
    }
    print!("{LAUNCH_ZSH}");
    Ok(0)
}

fn absolute(path: PathBuf) -> PathBuf {
    std::path::absolute(&path).unwrap_or(path)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

/// The work tree's root and the common git directory of the repository at
/// or above `start`: the nearest directory holding `.git`, read from its
/// layout, not above a `GIT_CEILING_DIRECTORIES` entry. Git is asked (one
/// process) only when the layout does not say, or `GIT_DIR` /
/// `GIT_WORK_TREE` point elsewhere.
fn repo_at(start: &Path) -> Result<(PathBuf, PathBuf)> {
    let start = absolute(start.to_path_buf());
    let set = |var: &str| std::env::var_os(var).is_some_and(|value| !value.is_empty());
    if !set("GIT_DIR") && !set("GIT_WORK_TREE") {
        let ceilings: Vec<PathBuf> = std::env::var_os("GIT_CEILING_DIRECTORIES")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        for dir in start.ancestors() {
            if dir != start && ceilings.iter().any(|ceiling| home::same_path(ceiling, dir)) {
                break;
            }
            if dir.join(".git").exists() {
                if let Some(common) = common_dir_in(dir, None, None) {
                    return Ok((dir.to_path_buf(), common));
                }
                break;
            }
        }
    }
    let text = home::git_value(
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ],
        Some(&start),
    )
    .with_context(|| format!("{} is not inside a git repository", start.display()))?
    .with_context(|| format!("{} is not inside a git repository", start.display()))?;
    let mut lines = text.lines();
    match (lines.next(), lines.next()) {
        (Some(top), Some(common)) => Ok((PathBuf::from(top), PathBuf::from(common))),
        _ => bail!("git named no work tree for {}", start.display()),
    }
}

/// The URL `git push <remote>` pushes to, as git works it out: the
/// `pushurl` with `insteadOf`, else the `url` with `pushInsteadOf`, else with
/// `insteadOf`.
fn push_url(listing: &Listing, remote: &str) -> Option<String> {
    let rewrites = listing.rewrites();
    if let Some(push) = listing.last(&format!("remote.{remote}.pushurl")) {
        return Some(gate::rewrite(&push.value, &rewrites));
    }
    let url = &listing.last(&format!("remote.{remote}.url"))?.value;
    let push_rules: Vec<gate::Rewrite> = listing
        .0
        .iter()
        .filter_map(|setting| {
            let base = setting
                .key
                .strip_prefix("url.")?
                .strip_suffix(".pushinsteadof")?;
            Some(gate::Rewrite {
                base: base.to_owned(),
                prefix: setting.value.clone(),
            })
        })
        .collect();
    let pushed = gate::rewrite(url, &push_rules);
    Some(if &pushed != url {
        pushed
    } else {
        gate::rewrite(url, &rewrites)
    })
}

/// The URL `git fetch <remote>` uses: its `url` with `insteadOf`.
fn fetch_url(listing: &Listing, remote: &str) -> Option<String> {
    let url = &listing.last(&format!("remote.{remote}.url"))?.value;
    Some(gate::rewrite(url, &listing.rewrites()))
}

/// What `doctor` says, line by line, and how much of it is drift.
struct Findings {
    drift: usize,
}

impl Findings {
    fn ok(&self, what: impl AsRef<str>) {
        println!("  ok     {}", what.as_ref());
    }
    fn drift(&mut self, what: impl AsRef<str>) {
        self.drift += 1;
        println!("  DRIFT  {}", what.as_ref());
    }
    fn note(&self, what: impl AsRef<str>) {
        println!("  note   {}", what.as_ref());
    }
}

/// An entry's hooks folder: which dispatchers there are, which are edited,
/// which are missing or spare, and whether what they run is still there.
fn check_dispatchers(
    found: &mut Findings,
    slug: &str,
    hooks: &Path,
    needed: &std::collections::BTreeSet<&'static str>,
) {
    let mut present = Vec::new();
    let mut ohjaas: Vec<PathBuf> = Vec::new();
    let mut syklis: Vec<Option<PathBuf>> = Vec::new();
    for name in HOOK_NAMES {
        let path = hooks.join(name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        match dispatcher_target(name, &text) {
            Some(dispatch) => {
                present.push(name);
                if !ohjaas.contains(&dispatch.ohjaa) {
                    ohjaas.push(dispatch.ohjaa);
                }
                if name == "pre-push" {
                    syklis.push(dispatch.sykli);
                }
            }
            None => found.drift(format!(
                "{} is not one of apply's dispatchers (edited?) (ohjaa apply)",
                path.display()
            )),
        }
        if !gate::executable(&path) {
            found.drift(format!(
                "{} is not executable (ohjaa apply)",
                path.display()
            ));
        }
    }
    let missing: Vec<&str> = needed
        .iter()
        .copied()
        .filter(|name| !present.contains(name))
        .collect();
    let spare: Vec<&str> = present
        .iter()
        .copied()
        .filter(|name| !needed.contains(name))
        .collect();
    if !missing.is_empty() {
        found.drift(format!(
            "{slug}: dispatchers missing in {}: {} (ohjaa apply)",
            hooks.display(),
            missing.join(", ")
        ));
    }
    if !spare.is_empty() {
        found.note(format!(
            "{slug}: spare dispatchers in {}: {} (each costs a process; ohjaa apply --prune \
             removes them)",
            hooks.display(),
            spare.join(", ")
        ));
    }
    for ohjaa in &ohjaas {
        if gate::executable(ohjaa) {
            found.ok(format!(
                "{slug}: dispatchers ({}) exec {}",
                present.join(", "),
                ohjaa.display()
            ));
        } else {
            found.drift(format!(
                "{slug}: the dispatchers exec {}, which is gone: pushes to the entry's branch \
                 fail, and so do commits in clones that chain a hook of their own (ohjaa apply)",
                ohjaa.display()
            ));
        }
    }
    for sykli in syklis {
        match sykli {
            Some(sykli) if gate::executable(&sykli) => {}
            Some(sykli) => found.drift(format!(
                "{slug}: the pre-push dispatcher hands the gate to {}, which is gone: pushes \
                 to the entry's branch are refused (ohjaa apply)",
                sykli.display()
            )),
            None => found.drift(format!(
                "{slug}: the pre-push dispatcher names no sykli (it was not on PATH when apply \
                 ran), so a git client without your PATH refuses pushes to the entry's \
                 branch (ohjaa apply)"
            )),
        }
    }
}

/// Where the roots come from, for doctor to say.
fn roots_source(explicit: &[PathBuf]) -> String {
    if !explicit.is_empty() {
        return "given with --root".to_owned();
    }
    match crate::roots::config_path().filter(|path| path.is_file()) {
        Some(path) => format!("from {}", path.display()),
        None => "derived from the current directory: no roots file yet (ohjaa apply writes \
                 one)"
            .to_owned(),
    }
}

pub fn doctor(explicit_roots: &[PathBuf], git_root: Option<&Path>) -> Result<i32> {
    let home = absolute(home::ohjaa_home()?);
    let mut found = Findings { drift: 0 };
    println!("ohjaa doctor: the workflow home is {}", home.display());

    match gate::git_version() {
        Ok(version) if version >= gate::MIN_GIT => found.ok(format!(
            "git {}.{} scopes hooks by remote (includeIf hasconfig needs {}.{})",
            version.0,
            version.1,
            gate::MIN_GIT.0,
            gate::MIN_GIT.1
        )),
        Ok(version) => found.drift(format!(
            "git {}.{} cannot scope hooks by remote (includeIf hasconfig needs {}.{})",
            version.0,
            version.1,
            gate::MIN_GIT.0,
            gate::MIN_GIT.1
        )),
        Err(error) => found.drift(format!("{error:#}")),
    }
    let entries = gate::entry_names(&home)?;
    let global = match gate::GlobalFacts::read() {
        Ok(global) => Some(global),
        Err(error) => {
            found.drift(format!("cannot read git's global config: {error:#}"));
            None
        }
    };
    let include = gate::include_file(&home);
    if let Some(global) = &global {
        if global.includes(&include) {
            found.ok(format!(
                "git's global config includes {}",
                include.display()
            ));
        } else {
            found.drift(format!(
                "git's global config does not include {}: no repository runs ohjaa's hooks \
                 (ohjaa apply)",
                include.display()
            ));
        }
        match global.hooks_path() {
            None => found.ok("git's global core.hooksPath is unset"),
            Some((path, _, _)) => found.drift(format!(
                "git's global core.hooksPath is {path}: it runs in every repository and \
                 overrides ohjaa's per-repository one (ohjaa apply --force removes it)"
            )),
        }
    }
    // The generated files: as they should be, else (rarely) why not. An
    // unreadable one breaks every git command, the global read among them.
    let expected = match &global {
        Some(global) => gate::generated(&home, &global.rewrites())?,
        None => {
            let mut files = vec![(include.clone(), String::new())];
            files.extend(
                entries
                    .iter()
                    .map(|entry| (gate::entry_gated(&entry.dir), String::new())),
            );
            files
        }
    };
    for (path, text) in &expected {
        if !path.is_file() {
            found.drift(format!(
                "{} is missing: the gate is off (ohjaa apply)",
                path.display()
            ));
        } else if global.is_some() && read_or_empty(path).is_ok_and(|now| &now == text) {
            found.ok(format!("{} is current", path.display()));
        } else if let Err(error) = gate::check_git_config(path) {
            found.drift(format!(
                "{} cannot be read by git, which then fails in every repository (ohjaa \
                 apply): {}",
                path.display(),
                error.root_cause()
            ));
        } else if global.is_some() {
            found.drift(format!(
                "{} is stale: it does not match repos/ (ohjaa apply)",
                path.display()
            ));
        }
    }

    let roots = crate::roots::load(explicit_roots, git_root);
    let repos = crate::roots::git_repos(&roots)?;
    println!(
        "roots: {} ({})",
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
        roots_source(explicit_roots)
    );
    let gated = match gate::gated_clones(&home, &repos) {
        Ok(gated) => gated,
        Err(error) => {
            found.drift(format!("cannot read the clones under the roots: {error:#}"));
            Vec::new()
        }
    };
    let needed = gate::needed_hooks(&home, &gated)?;

    match global.as_ref().map(gate::GlobalFacts::excludes_file) {
        Some(Ok(path)) => match read_or_empty(&path).map(|text| (ignore_with_block(&text), text)) {
            Ok((Ok((with, _)), text)) if with == text => found.ok(format!(
                "{} ignores {} for every repository",
                path.display(),
                IGNORED.join(" ")
            )),
            Ok((Ok(_), text)) if text.contains(IGNORE_BEGIN) => found.drift(format!(
                "{} holds an older ohjaa block; it should ignore {} (ohjaa apply)",
                path.display(),
                IGNORED.join(" ")
            )),
            Ok((Ok(_), _)) => found.drift(format!(
                "{} lacks the ohjaa block ({}) (ohjaa apply)",
                path.display(),
                IGNORED.join(" ")
            )),
            Ok((Err(why), _)) => found.drift(format!("{}: {why}", path.display())),
            Err(error) => found.drift(format!("{error:#}")),
        },
        Some(Err(error)) => {
            found.drift(format!("cannot find git's global excludes file: {error:#}"))
        }
        None => {}
    }
    match home::zshrc().and_then(|path| read_or_empty(&path).map(|text| (path, text))) {
        Ok((path, text)) => {
            let targets: Vec<PathBuf> = text.lines().filter_map(launch_target).collect();
            if targets.is_empty() {
                found.drift(format!(
                    "{} does not load the launcher (ohjaa apply)",
                    path.display()
                ));
            }
            for target in targets {
                if target == Path::new("ohjaa") {
                    found.drift(format!(
                        "{} loads the launcher unguarded, through PATH (ohjaa apply)",
                        path.display()
                    ));
                } else if gate::executable(&target) {
                    found.ok(format!(
                        "{} loads the launcher from {}",
                        path.display(),
                        target.display()
                    ));
                } else {
                    found.drift(format!(
                        "{} loads the launcher from {}, which is gone (ohjaa apply)",
                        path.display(),
                        target.display()
                    ));
                }
            }
            for (at, line) in text.lines().enumerate() {
                if sources_old_launcher(line) {
                    found.drift(format!(
                        "{} line {} still loads the old launcher: {} (ohjaa apply)",
                        path.display(),
                        at + 1,
                        line.trim()
                    ));
                }
            }
        }
        Err(error) => found.drift(format!("cannot read ~/.zshrc: {error:#}")),
    }

    println!("entries ({}):", home.join("repos").display());
    let mut readable = 0;
    for entry in &entries {
        let slug = format!("{}/{}", entry.owner, entry.name);
        match home::read_entry(&entry.dir) {
            Ok(Some(read)) if read.has_contract() => {
                readable += 1;
                found.ok(format!("{slug}: gates {}", read.branch));
            }
            Ok(Some(read)) => found.drift(format!(
                "{slug}: no gate ({} is missing)",
                read.contract().display()
            )),
            Ok(None) => {}
            Err(error) => found.drift(format!("{slug}: {error:#}")),
        }
        if let Some(needed) = needed.get(&entry.dir) {
            check_dispatchers(&mut found, &slug, &gate::entry_hooks(&entry.dir), needed);
        }
    }
    if readable == 0 && entries.is_empty() {
        found.note("no repository has an entry yet (ohjaa adopt <repo>)");
    }

    println!("repositories:");
    let mut plain = Vec::new();
    for repo in &repos {
        let label = repo
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| repo.display().to_string());
        if let Some(clone) = gated.iter().find(|clone| &clone.path == repo) {
            let named = format!("{label} ({})", clone.slugs.join(", "));
            let drift = gate::clone_drift(&home, clone);
            if drift.is_empty() {
                found.ok(format!(
                    "{named}: gated; git runs {} here{}",
                    clone.hooks_path.as_deref().unwrap_or_default(),
                    if clone.own.is_empty() {
                        String::new()
                    } else {
                        format!(", chaining to its own {}", clone.own.join(", "))
                    }
                ));
            }
            for line in drift {
                found.drift(format!("{named}: {line}"));
            }
        } else if repo.join("sykli.json").is_file() {
            found.note(format!(
                "{label}: an older in-repo sykli.json and no entry (ohjaa adopt {})",
                repo.display()
            ));
        } else {
            plain.push(label);
        }
    }
    if !plain.is_empty() {
        println!(
            "  -      not gated (no entry; git never runs ohjaa there): {}",
            plain.join(", ")
        );
    }
    if found.drift == 0 {
        println!("ohjaa doctor: healthy");
        Ok(0)
    } else {
        println!(
            "ohjaa doctor: {} drift{}",
            found.drift,
            if found.drift == 1 { "" } else { "s" }
        );
        Ok(DRIFT)
    }
}

/// The gate of the repository at `top`: the contract of the entry origin's
/// push URL names, else the repository's own `sykli.json`; with whose it is.
fn gate_of(top: &Path, listing: &Listing) -> Result<Option<(PathBuf, String)>> {
    let home = absolute(home::ohjaa_home()?);
    if let Some(id) = push_url(listing, "origin")
        .as_deref()
        .and_then(RepoId::parse)
    {
        match home::find_entry(&home, &id)? {
            Some(entry) if entry.has_contract() => {
                return Ok(Some((
                    entry.contract(),
                    format!("the entry for {} (branch {})", id.slug(), entry.branch),
                )))
            }
            Some(entry) => bail!(
                "{} has an entry but no gate: {} is missing (ohjaa adopt --force {})",
                id.slug(),
                entry.contract().display(),
                top.display()
            ),
            None => {}
        }
    }
    let own = top.join("sykli.json");
    Ok(own
        .is_file()
        .then(|| (own, "the repository's own sykli.json (no entry)".to_owned())))
}

/// `sykli <command> --help` names both facts flags: then sykli is handed the
/// repository's facts instead of asking git for them again.
fn sykli_takes_facts(command: &str, top: &Path, common: &Path) -> Vec<std::ffi::OsString> {
    let help = Command::new("sykli")
        .args([command, "--help"])
        .stdin(std::process::Stdio::null())
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    facts_for_sykli(&help, Some(top), common)
}

fn shown(words: &[std::ffi::OsString]) -> String {
    words
        .iter()
        .map(|word| {
            let word = word.to_string_lossy();
            if word.starts_with('-') {
                word.into_owned()
            } else {
                quote_path(Path::new(word.as_ref()))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `ohjaa gate`: run the gate for the repository here, the way a push will
/// judge it, and leave the receipt the push looks for. One git process (the
/// repository's config); the rest is sykli's.
pub fn gate_here(print_only: bool) -> Result<i32> {
    let cwd = std::env::current_dir().context("cannot read the current directory")?;
    let (top, common) = repo_at(&cwd)?;
    let listing = gate::clone_listing(&top)?;
    let Some((contract, whose)) = gate_of(&top, &listing)? else {
        eprintln!(
            "ohjaa gate: {} has no gate: origin names no entry (ohjaa adopt), and there is \
             no sykli.json in the repository",
            top.display()
        );
        return Ok(2);
    };
    let quoted = quote_path(&contract);
    let (run_facts, verify_facts) = if print_only {
        (Vec::new(), Vec::new())
    } else {
        (
            sykli_takes_facts("run", &top, &common),
            sykli_takes_facts("verify", &top, &common),
        )
    };
    let extra = |facts: &[std::ffi::OsString]| {
        if facts.is_empty() {
            String::new()
        } else {
            format!(" {}", shown(facts))
        }
    };
    let run = format!(
        "sykli run {quoted}{} --json > .sykli/receipt.json",
        extra(&run_facts)
    );
    let verify = format!(
        "sykli verify .sykli/receipt.json --contract {quoted}{} --commit HEAD",
        extra(&verify_facts)
    );
    println!("ohjaa gate: the gate is {} ({whose})", contract.display());
    if print_only {
        println!(
            "cd {} && mkdir -p .sykli && {run} && {verify}",
            quote_path(&top)
        );
        return Ok(0);
    }
    println!("ohjaa gate: in {}", top.display());
    let sykli_dir = top.join(".sykli");
    std::fs::create_dir_all(&sykli_dir)
        .with_context(|| format!("cannot create {}", sykli_dir.display()))?;
    println!("$ {run}");
    let receipt_path = sykli_dir.join("receipt.json");
    let receipt = std::fs::File::create(&receipt_path)
        .with_context(|| format!("cannot write {}", receipt_path.display()))?;
    let status = Command::new("sykli")
        .arg("run")
        .arg(&contract)
        .args(&run_facts)
        .arg("--json")
        .current_dir(&top)
        .stdout(receipt)
        .status()
        .context("cannot run sykli")?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        println!("ohjaa gate: failed: sykli run exited {code}; the receipt says why");
        return Ok(code);
    }
    println!("$ {verify}");
    let status = Command::new("sykli")
        .args(["verify", ".sykli/receipt.json", "--contract"])
        .arg(&contract)
        .args(&verify_facts)
        .args(["--commit", "HEAD"])
        .current_dir(&top)
        .status()
        .context("cannot run sykli")?;
    let code = status.code().unwrap_or(1);
    if status.success() {
        println!("ohjaa gate: passed: .sykli/receipt.json verifies for HEAD, so it can be pushed");
    } else {
        println!("ohjaa gate: failed: sykli verify exited {code}");
    }
    Ok(code)
}

/// An object `git cat-file --batch` found: its id and content.
type Object = (String, Vec<u8>);

/// What one `git cat-file --batch` says of each object asked for: its id
/// and content, or `None` when missing.
fn objects(top: &Path, names: &[String]) -> Result<Vec<Option<Object>>> {
    let mut command = Command::new("git");
    command.args(["cat-file", "--batch"]).current_dir(top);
    let input = names.join("\n") + "\n";
    let output = process::run(&mut command, Some(input.as_bytes()), process::LOCAL)
        .context("cannot run git cat-file")?;
    if !output.status.success() {
        bail!("git cat-file --batch failed: {}", output.failure_detail());
    }
    let mut out = output.stdout.as_slice();
    let mut found = Vec::new();
    for _ in names {
        let end = out
            .iter()
            .position(|byte| *byte == b'\n')
            .context("git cat-file --batch said less than asked")?;
        let header = String::from_utf8_lossy(&out[..end]).into_owned();
        out = &out[end + 1..];
        let fields: Vec<&str> = header.split(' ').collect();
        match fields[..] {
            [oid, _, size] if size.parse::<usize>().is_ok() => {
                let size: usize = size.parse().unwrap_or(0);
                if out.len() < size + 1 {
                    bail!("git cat-file --batch cut an object short");
                }
                found.push(Some((oid.to_owned(), out[..size].to_vec())));
                out = &out[size + 1..];
            }
            _ => found.push(None),
        }
    }
    Ok(found)
}

/// The branch `refs/remotes/origin/HEAD` names: the loose symbolic ref, else
/// (a reftable repository) asked of git.
fn origin_head(top: &Path, common: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(common.join("refs/remotes/origin/HEAD")) {
        Ok(text) => Ok(text
            .trim()
            .strip_prefix("ref: refs/remotes/origin/")
            .map(str::to_owned)),
        Err(_) if common.join("reftable").exists() => Ok(home::git_value(
            &["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"],
            Some(top),
        )?
        .and_then(|head| head.strip_prefix("refs/remotes/origin/").map(str::to_owned))),
        Err(_) => Ok(None),
    }
}

fn sykli_init_takes_a_contract() -> bool {
    Command::new("sykli")
        .args(["init", "--help"])
        .stdin(std::process::Stdio::null())
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("--contract"))
}

pub fn adopt(path: &Path, branch: Option<&str>, force: bool) -> Result<i32> {
    let (top, common) = repo_at(path)?;
    let listing = gate::clone_listing(&top)?;
    let url = fetch_url(&listing, "origin")
        .with_context(|| format!("{} has no origin remote to name it by", top.display()))?;
    let id = RepoId::parse(&url)
        .with_context(|| format!("origin's URL {url:?} names no owner and repository"))?;
    let branch = match branch {
        Some(branch) => branch
            .strip_prefix("refs/heads/")
            .unwrap_or(branch)
            .to_owned(),
        None => origin_head(&top, &common)?.with_context(|| {
            format!(
                "origin/HEAD is not set in {}, so its default branch is unknown here; \
                 name it with --branch, or set it with: git remote set-head origin --auto",
                top.display()
            )
        })?,
    };
    let home = absolute(home::ohjaa_home()?);
    let dir = home::entry_dir(&home, &id);
    if dir.join("repo.toml").exists() && !force {
        eprintln!(
            "ohjaa adopt: refusing: {} is already adopted at {}; rewrite it with --force",
            id.slug(),
            dir.display()
        );
        return Ok(REFUSED);
    }
    let created = !dir.exists();
    let result = write_entry(&top, &id, &dir, &branch, force);
    if result.is_err() && created {
        let _ = std::fs::remove_dir_all(&dir);
    }
    let source = result?;
    println!("ohjaa adopt: {} -> {}", id, dir.display());
    println!("  branch = {branch:?} (repo.toml)");
    println!("  {source}");
    // The home's git config now names this repository too; git's global
    // config is not touched (it includes the home's file once, from apply).
    let global = gate::GlobalFacts::read()?;
    let own = gate::own_hooks_in(&common);
    for said in crate::apply::regenerate(&home, &dir, &own, &global.rewrites())? {
        println!("  {said}");
    }
    match gate::gated_clone(&home, &top)? {
        Some(clone) => {
            let drift = gate::clone_drift(&home, &clone);
            if drift.is_empty() {
                println!(
                    "  gated: git runs {} here",
                    clone.hooks_path.as_deref().unwrap_or_default()
                );
            }
            for line in drift {
                eprintln!("ohjaa adopt: warning: {}: {line}", top.display());
            }
        }
        None => eprintln!(
            "ohjaa adopt: warning: no remote of {} names {} after all",
            top.display(),
            id.slug()
        ),
    }
    Ok(0)
}

/// Write the gate and `repo.toml` into `dir`; say where the gate came from.
/// The gate is read as `origin/<branch>` holds it (else `HEAD`), in one `git
/// cat-file --batch`.
fn write_entry(top: &Path, id: &RepoId, dir: &Path, branch: &str, force: bool) -> Result<String> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let revs = [format!("refs/remotes/origin/{branch}"), "HEAD".to_owned()];
    let files = ["sykli.json", "sykli.lock", "sykli.rs"];
    let mut names = Vec::new();
    for rev in &revs {
        names.push(rev.clone());
        names.extend(files.iter().map(|file| format!("{rev}:{file}")));
    }
    let found = objects(top, &names)?;
    let at = if found[0].is_some() {
        0
    } else {
        files.len() + 1
    };
    let rev = if at == 0 {
        format!("origin/{branch}")
    } else {
        "HEAD".to_owned()
    };
    let commit = found[at]
        .as_ref()
        .map(|(oid, _)| short(oid).to_owned())
        .unwrap_or_default();
    let blob = |n: usize| found[at + 1 + n].as_ref().map(|(_, bytes)| bytes.clone());
    let contract = dir.join("sykli.json");
    let lock = dir.join("sykli.lock");
    let source = if let Some(json) = blob(0) {
        std::fs::write(&contract, json)
            .with_context(|| format!("cannot write {}", contract.display()))?;
        match blob(1) {
            Some(bytes) => {
                std::fs::write(&lock, bytes)
                    .with_context(|| format!("cannot write {}", lock.display()))?;
                format!("sykli.json and sykli.lock copied from {rev} ({commit})")
            }
            None => {
                let _ = std::fs::remove_file(&lock);
                format!(
                    "sykli.json copied from {rev} ({commit}), which has no sykli.lock; \
                     pin it with: sykli lock {}",
                    contract.display()
                )
            }
        }
    } else if blob(2).is_some() {
        bail!(
            "{} gates with sykli.rs at {rev}; an entry holds a sykli.json contract, so write \
             one (sykli init --contract {}) and adopt again",
            id.slug(),
            contract.display()
        );
    } else {
        if !sykli_init_takes_a_contract() {
            bail!(
                "{} has no sykli.json at {rev}, and the sykli on PATH cannot write a contract \
                 outside the repository (`sykli init --help` names no --contract); nothing \
                 was adopted",
                id.slug()
            );
        }
        let mut init = Command::new("sykli");
        init.args(["init", "--contract"])
            .arg(&contract)
            .current_dir(top);
        if force && contract.exists() {
            init.arg("--force");
        }
        let output = process::run(&mut init, None, process::LOCAL).context("cannot run sykli")?;
        if !output.status.success() {
            bail!("sykli init failed: {}", output.failure_detail());
        }
        if !contract.is_file() {
            bail!("sykli init did not write {}", contract.display());
        }
        format!(
            "sykli.json written by sykli init --contract (no sykli.json at {rev}){}",
            if lock.is_file() {
                ", with sykli.lock"
            } else {
                ""
            }
        )
    };
    home::write_file(&dir.join("repo.toml"), &home::repo_toml(id, branch), None)?;
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launcher_is_the_one_the_person_wrote() {
        // The rules are verbatim; only the header says how it is loaded now.
        for rule in [
            "[[ ${OHJAA_WRAP:-1} == 0 || -n $OHJAA_SESSION ]] && return 1",
            "_ohjaa_at_terminal || return 1",
            "git rev-parse --git-dir >/dev/null 2>&1 || [[ $PWD == $HOME/projects* ]] || return 1",
            "claude:-p|claude:--print|claude:-c|claude:--continue|claude:-r|claude:--resume) return 1 ;;",
            "_ohjaa_task=(--task \"$1\")",
            "command claude \"$@\"",
        ] {
            assert!(LAUNCH_ZSH.contains(rule), "{rule}");
        }
        assert!(LAUNCH_ZSH.contains("shell-init zsh"));
        assert!(!LAUNCH_ZSH.contains("source ~/projects/ohjaa-launch.zsh"));
    }

    #[test]
    fn push_and_fetch_urls_are_worked_out_the_way_git_does() {
        let listing = Listing::parse(
            "url.git@github.com:.insteadof\ngh:\0\
             url.ssh://push.example/.pushinsteadof\nhttps://h/\0\
             remote.origin.url\ngh:acme/w\0\
             remote.up.url\nhttps://h/o/r\0\
             remote.fork.url\nhttps://h/me/r\0\
             remote.fork.pushurl\ngh:o/p\0",
            false,
        );
        assert_eq!(
            fetch_url(&listing, "origin").as_deref(),
            Some("git@github.com:acme/w")
        );
        assert_eq!(
            push_url(&listing, "origin").as_deref(),
            Some("git@github.com:acme/w")
        );
        assert_eq!(
            push_url(&listing, "up").as_deref(),
            Some("ssh://push.example/o/r")
        );
        assert_eq!(fetch_url(&listing, "up").as_deref(), Some("https://h/o/r"));
        assert_eq!(
            push_url(&listing, "fork").as_deref(),
            Some("git@github.com:o/p")
        );
        assert_eq!(push_url(&listing, "none"), None);
    }
}
