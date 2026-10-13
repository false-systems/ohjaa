//! Marks: what each commit on an entry's branch is, derived now and stored
//! never. Nothing is written into a repository, and ohjaa keeps nothing.
//!
//! - ✓ proven: a receipt of its tree verifies, `sykli verify <receipt>
//!   --contract <the entry's gate> --commit <sha>`;
//! - ✗ failed: a receipt of its tree is here, and it did not pass;
//! - ? unproven: no receipt of its tree is here, or sykli could not say.
//!
//! The walk starts where this clone last saw origin's copy of the entry's
//! branch (`refs/remotes/origin/<branch>`: no fetch, no network) and goes back
//! along first parents to the first proven commit, at most [`MAX_WALKED`]
//! commits, tip first. Shas and trees come from one git process; receipts are
//! read as files, from the layout: those sykli keeps for every push it
//! accepted (`<common>/sykli/receipts`) and, in every worktree, those `sykli
//! run` writes (`.sykli/receipts/`) and the one `ohjaa gate` saves
//! (`.sykli/receipt.json`). sykli is asked only about a commit with a receipt
//! of its tree that says it passed. A merge commit a forge made from an
//! up-to-date pull request has the tree of the pull request's head, so a
//! receipt of the head proves the merge.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::gitconfig::{self, Listing};
use crate::home::{self, RepoId};
use crate::lifecycle::{self, Gate, Receipts, Sykli};
use crate::process;
use crate::sessions::{self, Check};

/// The most commits a walk reads.
pub const MAX_WALKED: usize = 50;

/// The gate of the clone at `top` (common git dir `common`): the entry
/// origin's push URL names, when it has a contract; from the clone's config.
pub fn gate_of(home: &Path, top: &Path, common: &Path, listing: &Listing) -> Option<Gate> {
    let id = gitconfig::push_url(listing, "origin")
        .as_deref()
        .and_then(RepoId::parse)?;
    let entry = home::find_entry(home, &id).ok()??;
    entry.has_contract().then(|| Gate {
        slug: id.slug(),
        branch: entry.branch.clone(),
        contract: home::absolute(entry.contract()),
        top: top.to_path_buf(),
        common: common.to_path_buf(),
    })
}

/// The gate of the repository `repo` lies in: one git process (its config)
/// for a clone whose config file names an entry's `owner/name` (or includes
/// another file), none for any other clone, nor when the home has no entry.
pub fn gate_here(home: &Path, repo: &Path) -> Option<Gate> {
    let entries = gitconfig::entry_names(home).ok()?;
    if entries.is_empty() {
        return None;
    }
    let (top, common) = gitconfig::repo_at(repo).ok()?;
    let config = std::fs::read_to_string(common.join("config"))
        .unwrap_or_default()
        .to_lowercase();
    let named = config.contains("[include")
        || entries.iter().any(|entry| {
            config.contains(&format!("{}/{}", entry.owner, entry.name).to_lowercase())
        });
    if !named {
        return None;
    }
    let listing = gitconfig::clone_listing(&top).ok()?;
    gate_of(home, &top, &common, &listing)
}

/// One commit on the branch, and its mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Walked {
    pub commit: String,
    pub tree: String,
    pub author: String,
    /// Author time, unix seconds.
    pub authored: i64,
    pub subject: String,
    pub check: Check,
}

/// Why a walk stopped where it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// At a proven commit, the last one listed.
    Proven,
    /// At the branch's first commit: nothing on it is proven.
    Root,
    /// After [`MAX_WALKED`] commits, none of them proven.
    Cap,
    /// Before a commit only sykli could judge, with no `sykli verify` left.
    NotAsked,
}

impl Ended {
    pub fn word(self) -> &'static str {
        match self {
            Ended::Proven => "proven",
            Ended::Root => "root",
            Ended::Cap => "cap",
            Ended::NotAsked => "not_asked",
        }
    }
}

/// The marks on an entry's branch in one clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marks {
    pub gate: Gate,
    /// When this clone last fetched (`FETCH_HEAD`), in milliseconds.
    pub fetched_ms: Option<i64>,
    /// Tip first.
    pub commits: Vec<Walked>,
    pub ended: Ended,
}

impl Marks {
    pub fn proven_at_tip(&self) -> bool {
        self.commits
            .first()
            .is_some_and(|tip| tip.check == Check::Passed)
    }

    /// The commits above the first proven one, tip first.
    pub fn unproven(&self) -> &[Walked] {
        let proven = self
            .commits
            .iter()
            .position(|walked| walked.check == Check::Passed)
            .unwrap_or(self.commits.len());
        &self.commits[..proven]
    }
}

/// ✓, ✗ or ?.
pub fn symbol(check: &Check) -> &'static str {
    match check {
        Check::Passed => "✓",
        Check::Failed { .. } => "✗",
        _ => "?",
    }
}

/// `proven`, `failed` or `unproven`.
pub fn word(check: &Check) -> &'static str {
    match check {
        Check::Passed => "proven",
        Check::Failed { .. } => "failed",
        _ => "unproven",
    }
}

/// When the clone whose common git dir is `common` last fetched, in
/// milliseconds: `FETCH_HEAD`'s time.
pub fn fetched(common: &Path) -> Option<i64> {
    let modified = std::fs::metadata(common.join("FETCH_HEAD"))
        .ok()?
        .modified()
        .ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// The marks on `gate`'s branch, against `receipts`: one git process, and
/// `sykli verify` only for a commit with a receipt of its tree that says it
/// passed, while `sykli` may still verify.
pub fn walk(gate: &Gate, sykli: &Sykli, receipts: &Receipts) -> Result<Marks, String> {
    let reference = format!("refs/remotes/origin/{}", gate.branch);
    let output = process::run(
        Command::new("git").arg("-C").arg(&gate.top).args([
            "log",
            "--first-parent",
            &format!("--max-count={MAX_WALKED}"),
            "--format=%H%x1f%T%x1f%an%x1f%at%x1f%s",
            &reference,
            "--",
        ]),
        None,
        process::LOCAL,
    )
    .map_err(|error| format!("git could not be asked: {error:#}"))?;
    if !output.status.success() {
        return Err(format!(
            "origin/{} cannot be read in {} ({})",
            gate.branch,
            gate.top.display(),
            output.failure_detail()
        ));
    }
    let mut commits = Vec::new();
    let mut ended = None;
    for line in output.stdout_text().lines() {
        let mut fields = line.splitn(5, '\u{1f}');
        let (Some(commit), Some(tree), Some(author), Some(authored), subject) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next().unwrap_or(""),
        ) else {
            continue;
        };
        let asks = receipts
            .of(tree)
            .iter()
            .any(|receipt| matches!(receipt.evidence.outcome.as_str(), "passed" | "cached"));
        if asks && !sykli.may_verify() {
            ended = Some(Ended::NotAsked);
            break;
        }
        let check = sykli.check_gated(gate, commit, tree, receipts, true);
        let proven = check == Check::Passed;
        commits.push(Walked {
            commit: commit.to_owned(),
            tree: tree.to_owned(),
            author: author.to_owned(),
            authored: authored.parse().unwrap_or(0),
            subject: subject.to_owned(),
            check,
        });
        if proven {
            ended = Some(Ended::Proven);
            break;
        }
    }
    let ended = ended.unwrap_or(if commits.len() >= MAX_WALKED {
        Ended::Cap
    } else {
        Ended::Root
    });
    Ok(Marks {
        gate: gate.clone(),
        fetched_ms: fetched(&gate.common),
        commits,
        ended,
    })
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn as_of(marks: &Marks) -> String {
    marks.fetched_ms.map_or_else(
        || "never fetched here".to_owned(),
        |ms| format!("as of last fetch {}", sessions::clock(ms)),
    )
}

/// The flag a briefing opens with when the entry's branch is not proven at
/// its tip; `None` when it is, or when there is nothing on it.
pub fn flag(marks: &Marks) -> Option<String> {
    let unproven = marks.unproven();
    let oldest = unproven.last()?;
    let branch = &marks.gate.branch;
    let n = unproven.len();
    let mut listed: Vec<String> = unproven
        .iter()
        .take(5)
        .map(|walked| format!("{} {}", symbol(&walked.check), short(&walked.commit)))
        .collect();
    if n > 5 {
        listed.push("…".to_owned());
    }
    let since = match marks.ended {
        Ended::Proven => format!("is unproven since {}", short(&oldest.commit)),
        Ended::Root => "has never been proven".to_owned(),
        Ended::Cap => format!("is unproven in each of its last {n} commits"),
        Ended::NotAsked => format!("is unproven since at least {}", short(&oldest.commit)),
    };
    let commits = if n == 1 { "commit" } else { "commits" };
    let count = if marks.ended == Ended::Cap {
        String::new()
    } else {
        format!("{n} {commits}: ")
    };
    Some(format!(
        "FLAG: {branch} of {} {since} ({count}{}; {}). Start by running `ohjaa gate` on \
         {branch} and fix forward what fails.",
        marks.gate.slug,
        listed.join(", "),
        as_of(marks)
    ))
}

/// The marks of one entry, for `ohjaa report`, in one line: tip first.
pub fn line(marks: &Marks) -> String {
    let mut out = format!(
        "{} {} ({}):",
        marks.gate.slug,
        marks.gate.branch,
        as_of(marks)
    );
    for walked in &marks.commits {
        let _ = write!(out, " {} {}", symbol(&walked.check), short(&walked.commit));
    }
    match marks.ended {
        Ended::Proven => {}
        Ended::Root => out.push_str(" (nothing on it is proven)"),
        Ended::Cap => {
            let _ = write!(out, " (none proven in the newest {MAX_WALKED})");
        }
        Ended::NotAsked => out.push_str(" (sykli not asked further)"),
    }
    out
}

/// Every entry in `home`, with the marks on its branch, or why there are
/// none. An entry is marked in the clones under the roots (`repos`) whose
/// origin names it: a clone's config file must name the entry before git is
/// asked about it (one process), and the walk is one more. With several
/// clones of one entry, the walk is in the one that fetched last, against
/// the receipts of all of them.
pub fn of_entries(
    home: &Path,
    repos: &[PathBuf],
    sykli: &Sykli,
) -> Vec<(String, Result<Marks, String>)> {
    let entries = match home::entry_dirs(home) {
        Ok(entries) => entries,
        Err(error) => return vec![("(entries)".to_owned(), Err(format!("{error:#}")))],
    };
    if entries.is_empty() {
        return Vec::new();
    }
    let mut gates: Vec<Gate> = Vec::new();
    let mut commons = std::collections::HashSet::new();
    for repo in repos {
        let Ok((_, common)) = gitconfig::repo_at(repo) else {
            continue;
        };
        // A worktree of a clone already read is the same clone.
        if !commons.insert(std::fs::canonicalize(&common).unwrap_or(common)) {
            continue;
        }
        if let Some(gate) = sykli.gate_for(repo) {
            gates.push(gate);
        }
    }
    entries
        .into_iter()
        .map(|(slug, dir)| {
            let mine: Vec<&Gate> = gates
                .iter()
                .filter(|gate| gate.slug.eq_ignore_ascii_case(&slug))
                .collect();
            let marked = match mine.iter().max_by_key(|gate| fetched(&gate.common)) {
                Some(gate) => {
                    let mut receipts = Receipts::default();
                    for clone in &mine {
                        receipts.extend(lifecycle::receipts_of_layout(&clone.common));
                    }
                    walk(gate, sykli, &receipts)
                }
                None => Err(match home::read_entry(&dir) {
                    Ok(Some(entry)) if !entry.has_contract() => {
                        format!("it has no gate ({} is missing)", entry.contract().display())
                    }
                    Ok(_) => "no clone under the roots has it as origin".to_owned(),
                    Err(error) => format!("its entry cannot be read: {error:#}"),
                }),
            };
            (slug, marked)
        })
        .collect()
}

/// For `ohjaa run` and `ohjaa brief` in the repository at `top`: the
/// drift warnings doctor would give it, and the flag when the entry's branch
/// is not proven at its tip. Nothing at all without an entry; else one git
/// process for the clone's config, one for the walk, and `sykli verify` at
/// most once.
pub fn at_launch(top: &Path) -> (Vec<String>, Option<String>) {
    let Ok(home) = home::ohjaa_home() else {
        return (Vec::new(), None);
    };
    let home = home::absolute(home);
    let (warnings, gate) = gitconfig::launch_view(&home, top);
    let Some(gate) = gate else {
        return (warnings, None);
    };
    let sykli = Sykli::with_home(home);
    sykli.allow_verifies(1);
    let receipts = lifecycle::receipts_of_layout(&gate.common);
    let flag = walk(&gate, &sykli, &receipts)
        .ok()
        .and_then(|marks| flag(&marks));
    (warnings, flag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(checks: &[Check], ended: Ended) -> Marks {
        Marks {
            gate: Gate {
                slug: "acme/widget".to_owned(),
                branch: "main".to_owned(),
                contract: PathBuf::from("/h/sykli.json"),
                top: PathBuf::from("/r"),
                common: PathBuf::from("/r/.git"),
            },
            fetched_ms: None,
            commits: checks
                .iter()
                .enumerate()
                .map(|(n, check)| Walked {
                    commit: format!("{n}{n}{n}{n}{n}{n}{n}abc"),
                    tree: String::new(),
                    author: String::new(),
                    authored: 0,
                    subject: String::new(),
                    check: check.clone(),
                })
                .collect(),
            ended,
        }
    }

    #[test]
    fn the_flag_says_since_when_and_how_it_ended() {
        let failed = Check::Failed {
            why: "no".to_owned(),
        };
        let proven = marks(
            &[Check::Unchecked, failed.clone(), Check::Passed],
            Ended::Proven,
        );
        assert_eq!(
            flag(&proven).unwrap(),
            "FLAG: main of acme/widget is unproven since 1111111 (2 commits: ? 0000000, ✗ \
             1111111; never fetched here). Start by running `ohjaa gate` on main and fix \
             forward what fails."
        );
        assert_eq!(
            line(&proven),
            "acme/widget main (never fetched here): ? 0000000 ✗ 1111111 ✓ 2222222"
        );
        assert_eq!(flag(&marks(&[Check::Passed], Ended::Proven)), None);
        assert_eq!(flag(&marks(&[], Ended::Root)), None, "nothing on it");
        let root = marks(&[Check::Unchecked], Ended::Root);
        assert!(flag(&root)
            .unwrap()
            .starts_with("FLAG: main of acme/widget has never been proven (1 commit: ? "));
        assert!(line(&root).ends_with(" (nothing on it is proven)"));
        let many = vec![Check::Unchecked; 7];
        let capped = flag(&marks(&many, Ended::Cap)).unwrap();
        assert!(
            capped.starts_with(
                "FLAG: main of acme/widget is unproven in each of its last 7 commits (? "
            ) && capped.contains(", …; never fetched here)"),
            "{capped}"
        );
        let not_asked = flag(&marks(&[Check::Unchecked], Ended::NotAsked)).unwrap();
        assert!(
            not_asked.contains("is unproven since at least 0000000 (1 commit: "),
            "{not_asked}"
        );
    }
}
