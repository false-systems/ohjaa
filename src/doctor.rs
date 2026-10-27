//! `ohjaa doctor`: what differs from what `apply` would make it and, clone
//! by clone, whether git really runs ohjaa's hooks where an entry says it
//! should. Read-only.
//!
//! It reads its facts once: git's global config in one process, a clone's
//! config in one, its common directory from the layout.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::apply::{
    ignore_with_block, launch_target, read_or_empty, sources_old_launcher, IGNORED, IGNORE_BEGIN,
};
use crate::gitconfig;
use crate::home::{self, absolute};
use crate::hook::{dispatcher_target, HOOK_NAMES};
use crate::process;

/// Something differs from what `apply` would make it.
const DRIFT: i32 = 4;

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

/// Each agent's own instruction file, as apply would make it: the current
/// knowledge block, in each that exists outside every repository. One apply
/// cannot write (a block it cannot tell from the rest) is drift until a
/// person fixes it; a block apply did not write is the person's.
fn check_instructions(found: &mut Findings) {
    let compact = match crate::instructions::home_knowledge() {
        Ok((compact, _)) => compact,
        Err(error) => {
            found.drift(format!("cannot read mine and machine knowledge: {error:#}"));
            return;
        }
    };
    for target in crate::instructions::targets() {
        let Some(file) = target.file else {
            if target.installed {
                found.note(format!(
                    "{} reads {}, which does not exist: it gets no knowledge block",
                    target.agent,
                    target.usual.display()
                ));
            }
            continue;
        };
        let agent = target.agent;
        if crate::instructions::inside_repository(&file).is_some() {
            found.note(format!(
                "{} ({agent}'s own instructions) is inside a repository: it gets no knowledge block",
                file.display()
            ));
            continue;
        }
        use crate::instructions::Block;
        let read =
            read_or_empty(&file).and_then(|now| crate::instructions::block_in(&now, &compact));
        match read {
            Err(error) => found.drift(format!(
                "{} ({agent}'s own instructions): apply cannot write the knowledge block there: \
                 {error:#} (once it is fixed, ohjaa apply)",
                file.display()
            )),
            Ok(Block::Theirs) => found.note(format!(
                "{} ({agent}'s own instructions) holds a knowledge block apply did not write: \
                 the person's",
                file.display()
            )),
            Ok(Block::None) => found.drift(format!(
                "{} ({agent}'s own instructions) has no knowledge block (ohjaa apply)",
                file.display()
            )),
            Ok(Block::Current) => found.ok(format!(
                "{} ({agent}'s own instructions) holds the current knowledge block",
                file.display()
            )),
            Ok(Block::Stale) => found.drift(format!(
                "{} ({agent}'s own instructions): its knowledge block is not current (ohjaa apply)",
                file.display()
            )),
        }
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
        if !gitconfig::executable(&path) {
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
        if gitconfig::executable(ohjaa) {
            found.ok(format!(
                "{slug}: dispatchers ({}) exec {}",
                present.join(", "),
                ohjaa.display()
            ));
        } else {
            found.drift(format!(
                "{slug}: the dispatchers exec {}, which is gone: pushes in its clones fail, and \
                 so do commits in clones that chain a hook of their own (ohjaa apply)",
                ohjaa.display()
            ));
        }
    }
    for sykli in syklis {
        match sykli {
            Some(sykli) if gitconfig::executable(&sykli) => {}
            Some(sykli) => found.drift(format!(
                "{slug}: the pre-push dispatcher hands pushes to {}, which is gone: every push \
                 to the entry's branch is flagged unproven, never judged (ohjaa apply)",
                sykli.display()
            )),
            None => found.drift(format!(
                "{slug}: the pre-push dispatcher names no sykli (it was not on PATH when apply \
                 ran), so a git client without your PATH flags every push to the entry's \
                 branch unproven (ohjaa apply)"
            )),
        }
    }
}

/// The gate derived from the tip's own `sykli.json`: where it comes from,
/// the lines it gives, and whether the repository's own lock pins its
/// contract (a lock that does not prevents gate evidence, as it prevents the
/// repository's own: pushes are flagged UNPROVEN, never refused).
fn say_derived(found: &mut Findings, slug: &str, tip: &str, derived: &crate::derived::Derived) {
    let given = if !derived.given.is_empty() {
        format!("; [cargo_env] given to {}", derived.given.join(", "))
    } else if derived.unread.is_some() {
        String::new()
    } else if derived.cargo.is_empty() {
        "; it runs no cargo".to_owned()
    } else {
        "; no cargo task is given a [cargo_env] line".to_owned()
    };
    // A tip with no contract of its own: what sykli init writes for its tree.
    let (source, contract) = match &derived.inferred {
        None => (
            format!("{tip}'s own sykli.json"),
            "its sykli.json".to_owned(),
        ),
        Some(tree) => (
            format!(
                "what sykli init writes for {tip}'s tree ({}), which has no sykli.json of its own",
                tree.get(..7).unwrap_or(tree)
            ),
            "the contract sykli init wrote".to_owned(),
        ),
    };
    found.ok(format!(
        "{slug}: its gate is derived from {source}{given} (no copy is consulted)"
    ));
    if let Some(why) = &derived.unread {
        found.note(format!(
            "{slug}: {contract} is {why}: its gate is that file as it is, with no [cargo_env] \
             line"
        ));
    }
    match (derived.pinned, &derived.inferred) {
        (crate::derived::Pinned::Yes, None) => found.ok(format!(
            "{slug}: {tip}'s sykli.lock pins its sykli.json, so its gate is locked too"
        )),
        (crate::derived::Pinned::Yes, Some(_)) => found.ok(format!(
            "{slug}: sykli init locked the contract it wrote, so its gate is locked too"
        )),
        (crate::derived::Pinned::NoLock, None) => found.note(format!(
            "{slug}: {tip} has no sykli.lock, so its gate runs unpinned"
        )),
        (crate::derived::Pinned::NoLock, Some(_)) => found.note(format!(
            "{slug}: sykli init wrote no sykli.lock beside {contract}, so its gate runs unpinned"
        )),
        (crate::derived::Pinned::No, None) => found.drift(format!(
            "{slug}: {tip}'s sykli.lock does not pin its sykli.json: sykli refuses to run its \
             gate, as it refuses the repository's own; a push is flagged UNPROVEN, not refused. \
             Run sykli lock in the repository and land it on its observed branch"
        )),
        (crate::derived::Pinned::No, Some(_)) => found.drift(format!(
            "{slug}: the sykli.lock sykli init wrote does not pin {contract}: sykli refuses to \
             run its gate; a push is flagged UNPROVEN, not refused"
        )),
    }
}

/// KATANA D10 for a derived gate: the entry's `[cargo_env]` against the
/// syklis that run the gate. Lines naming `${worktree_cache}` while one of
/// them does not expand it build in a `${worktree_cache}/` folder inside
/// the work tree (drift); no line while every one does lets one worktree's
/// gate run what another built (drift; a note while one does not).
fn check_overlay(
    found: &mut Findings,
    (slug, tip): (&str, &str),
    overlay: &crate::derived::Overlay,
    derived: &crate::derived::Derived,
    home: &Path,
    expands: &mut Option<std::result::Result<(), String>>,
) {
    let mut expanded = || {
        expands
            .get_or_insert_with(|| crate::adopt::sykli_expands_worktree_cache(home))
            .clone()
    };
    // The repository's own tasks may name the placeholder too.
    if !derived.placeholder.is_empty() {
        if let Err(why) = expanded() {
            found.drift(format!(
                "{slug}: {} in {tip}'s own sykli.json name ${{worktree_cache}}, but {why}: it \
                 passes the placeholder on as written, so cargo builds in a \
                 `${{worktree_cache}}/` folder inside the work tree (update it)",
                derived.placeholder.join(", ")
            ));
        }
    }
    if derived.cargo.is_empty() {
        return;
    }
    if overlay.is_empty() {
        let lacking = format!(
            "{slug}: its [cargo_env] gives cargo tasks no line: gate runs may share a target \
             dir across worktrees"
        );
        match expanded() {
            Ok(()) => found.drift(format!("{lacking} (ohjaa adopt --force <its clone>)")),
            Err(why) => found.note(format!(
                "{lacking}, and {why} (update it, then ohjaa adopt --force)"
            )),
        }
        return;
    }
    let names = overlay
        .lines()
        .iter()
        .any(|(_, value)| value.contains("${worktree_cache}"));
    match expanded() {
        Err(why) if names => found.drift(format!(
            "{slug}: its [cargo_env] names ${{worktree_cache}}, but {why}: it passes the \
             placeholder on as written, so cargo builds in a `${{worktree_cache}}/` folder inside \
             the work tree (update it; ohjaa adopt --force <its clone> empties [cargo_env] \
             meanwhile)"
        )),
        _ => found.ok(format!(
            "{slug}: [cargo_env] gives each cargo task of its gate {} unless it says where it \
             builds",
            overlay
                .lines()
                .iter()
                .map(|(var, _)| var.as_str())
                .collect::<Vec<_>>()
                .join(" and ")
        )),
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

    match gitconfig::git_version() {
        Ok(version) if version >= gitconfig::MIN_GIT => found.ok(format!(
            "git {}.{} scopes hooks by remote (includeIf hasconfig needs {}.{})",
            version.0,
            version.1,
            gitconfig::MIN_GIT.0,
            gitconfig::MIN_GIT.1
        )),
        Ok(version) => found.drift(format!(
            "git {}.{} cannot scope hooks by remote (includeIf hasconfig needs {}.{})",
            version.0,
            version.1,
            gitconfig::MIN_GIT.0,
            gitconfig::MIN_GIT.1
        )),
        Err(error) => found.drift(format!("{error:#}")),
    }
    let entries = gitconfig::entry_names(&home)?;
    let global = match gitconfig::GlobalFacts::read() {
        Ok(global) => Some(global),
        Err(error) => {
            found.drift(format!("cannot read git's global config: {error:#}"));
            None
        }
    };
    let include = gitconfig::include_file(&home);
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
        Some(global) => gitconfig::generated(&home, &global.rewrites())?,
        None => {
            let mut files = vec![(include.clone(), String::new())];
            files.extend(
                entries
                    .iter()
                    .map(|entry| (gitconfig::entry_gated(&entry.dir), String::new())),
            );
            files
        }
    };
    for (path, text) in &expected {
        if !path.is_file() {
            found.drift(format!(
                "{} is missing: nothing is observed (ohjaa apply)",
                path.display()
            ));
        } else if global.is_some() && read_or_empty(path).is_ok_and(|now| &now == text) {
            found.ok(format!("{} is current", path.display()));
        } else if let Err(error) = gitconfig::check_git_config(path) {
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
    let gated = match gitconfig::gated_clones(&home, &repos) {
        Ok(gated) => gated,
        Err(error) => {
            found.drift(format!("cannot read the clones under the roots: {error:#}"));
            Vec::new()
        }
    };
    let needed = gitconfig::needed_hooks(&home, &gated)?;

    match global.as_ref().map(gitconfig::GlobalFacts::excludes_file) {
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
                if gitconfig::executable(&target) {
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
    check_instructions(&mut found);

    println!("entries ({}):", home.join("repos").display());
    let mut readable = 0;
    // Whether the syklis that run the gates expand ${worktree_cache}: asked
    // once, and only when a gate has a cargo task that lacks the lines or a
    // task that names the placeholder.
    let mut expands: Option<std::result::Result<(), String>> = None;
    // Where the gates sykli init writes are read from (as a run kept them),
    // else asked in a scratch checkout of this process's own: doctor keeps
    // nothing.
    let gates = crate::derived::Gates::of_state();
    for entry in &entries {
        let slug = format!("{}/{}", entry.owner, entry.name);
        match home::read_entry(&entry.dir) {
            Ok(Some(read)) => {
                readable += 1;
                found.ok(format!("{slug}: observes {}", read.branch));
                // The entry's clone under the roots, its facts read from its
                // layout.
                let clone = gated
                    .iter()
                    .find(|clone| clone.entries.contains(&entry.dir))
                    .and_then(|clone| gitconfig::repo_at(&clone.path).ok());
                // Which contract judges its branch's tip: derived from the
                // tip's own sykli.json, else from what sykli init writes for
                // its tree (asked of sykli in a scratch checkout of the tree,
                // two git processes more). One git process for the clone.
                let at = clone.as_ref().map(|(_, common)| {
                    crate::derived::Objects::of_clone(common).and_then(|mut objects| {
                        let tip = format!("refs/remotes/origin/{}", read.branch);
                        let commit = objects
                            .commit(&tip)?
                            .ok_or_else(|| format!("this clone has no {tip}"))?;
                        let holds = objects.holds_at(&commit.oid)?;
                        drop(objects);
                        Ok(crate::derived::GateAt::of(
                            &read.overlay,
                            &commit.oid,
                            holds,
                            |tree| gates.inferred(common, tree, Path::new("sykli"), process::LOCAL),
                        ))
                    })
                });
                let tip = format!("origin/{}", read.branch);
                match at {
                    Some(Ok(crate::derived::GateAt::Derived(derived))) => {
                        say_derived(&mut found, &slug, &tip, &derived);
                        check_overlay(
                            &mut found,
                            (&slug, &tip),
                            &read.overlay,
                            &derived,
                            &home,
                            &mut expands,
                        );
                    }
                    Some(Ok(crate::derived::GateAt::Missing(why))) => {
                        found.drift(format!("{slug}: no gate for {tip}: {why}"))
                    }
                    Some(Err(why)) => found.note(format!(
                        "{slug}: which contract judges {tip} is unknown: {why}"
                    )),
                    None => found.note(format!(
                        "{slug}: which contract judges {tip} is not known here (no clone under \
                         the roots); each commit is judged by the gate derived from it: its own \
                         sykli.json, else what sykli init writes for its tree"
                    )),
                }
                if read.has_stored() {
                    found.note(format!(
                        "{slug}: the copy {} is never read: every commit's gate is derived from \
                         the commit (ohjaa adopt --force removes it)",
                        read.stored().display()
                    ));
                }
            }
            Ok(None) => {}
            Err(error) => found.drift(format!("{slug}: {error:#}")),
        }
        if let Some(needed) = needed.get(&entry.dir) {
            check_dispatchers(
                &mut found,
                &slug,
                &gitconfig::entry_hooks(&entry.dir),
                needed,
            );
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
            let drift = gitconfig::clone_drift(&home, clone);
            if drift.is_empty() {
                found.ok(format!(
                    "{named}: observed; git runs {} here{}",
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
            "  -      not observed (no entry; git never runs ohjaa there): {}",
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
