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

    println!("entries ({}):", home.join("repos").display());
    let mut readable = 0;
    for entry in &entries {
        let slug = format!("{}/{}", entry.owner, entry.name);
        match home::read_entry(&entry.dir) {
            Ok(Some(read)) if read.has_contract() => {
                readable += 1;
                found.ok(format!("{slug}: observes {}", read.branch));
            }
            Ok(Some(read)) => found.drift(format!(
                "{slug}: no gate ({} is missing)",
                read.contract().display()
            )),
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
