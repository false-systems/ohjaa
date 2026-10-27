//! Marks: what each commit on an entry's branch is, derived now and stored
//! never. Nothing is written into a repository, and ohjaa keeps nothing.
//!
//! - ✓ proven: sykli accepted a receipt of its tree against the commit's
//!   gate, `sykli verify <receipt> --contract <its gate> --commit <sha>`, or
//!   against its own `sykli.json` for a run of that (receipts are unsigned
//!   files: this is what sykli accepted, not a run of the tree);
//! - ✗ failed: the newest run of its tree here failed;
//! - ? unproven: no receipt of its tree is here, it passed under another
//!   contract or lock than the commit's gate and its own, or sykli could not
//!   say.
//!
//! The walk starts where this clone last saw origin's copy of the entry's
//! branch (`refs/remotes/origin/<branch>`: no fetch, no network) and goes back
//! along first parents to the first proven commit, at most [`MAX_WALKED`]
//! commits, tip first. The commits, and the gate of each commit sykli is
//! asked about, come from one git process (the clone's `git cat-file
//! --batch`); receipts are
//! read as files, from the layout: those sykli keeps under the clone's
//! common dir, which outlive the worktree that ran them (the receipt of
//! every push its hook accepted, `<common>/sykli/receipts`, and the receipt
//! each run concluded with, in the run's own folder, `<common>/sykli/runs/
//! <run>/`) and, in every worktree, those `sykli run` writes
//! (`.sykli/receipts/`) and the one `ohjaa gate` saves
//! (`.sykli/receipt.json`). sykli is asked only about a commit with a receipt
//! of its tree that says it passed. A merge commit a forge made from an
//! up-to-date pull request has the tree of the pull request's head, so a
//! receipt of the head proves the merge, also once the worktree it was
//! proven in is removed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::derived::Under;
use crate::gitconfig::{self, Listing};
use crate::home::{self, RepoId};
use crate::lifecycle::{self, Gate, Receipts, Sykli};
use crate::sessions::{self, Check};

/// The most commits a walk reads.
pub const MAX_WALKED: usize = 50;

/// The gate of the clone at `top` (common git dir `common`): the entry
/// origin's push URL names; from the clone's config. Which contract judges
/// each commit is read from the commit ([`Sykli::gate_at`]).
pub fn gate_of(home: &Path, top: &Path, common: &Path, listing: &Listing) -> Option<Gate> {
    let id = gitconfig::push_url(listing, "origin")
        .as_deref()
        .and_then(RepoId::parse)?;
    let entry = home::find_entry(home, &id).ok()??;
    Some(gate_for_entry(&id, &entry, top, common))
}

/// The gate `entry` (of the repository `id`) gives the clone at `top`.
pub fn gate_for_entry(id: &RepoId, entry: &home::Entry, top: &Path, common: &Path) -> Gate {
    Gate {
        slug: id.slug(),
        branch: entry.branch.clone(),
        entry: home::absolute(entry.dir.clone()),
        overlay: entry.overlay.clone(),
        top: top.to_path_buf(),
        common: common.to_path_buf(),
    }
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
            names(
                &config,
                &format!("{}/{}", entry.owner, entry.name).to_lowercase(),
            )
        });
    if !named {
        return None;
    }
    let listing = gitconfig::clone_listing(&top).ok()?;
    gate_of(home, &top, &common, &listing)
}

/// Whether `text` names `owner/name` as a whole: `acme/r1` in
/// `…/acme/r1.git`, never in `…/acme/r10` or `…/xacme/r1`.
fn names(text: &str, slug: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    text.match_indices(slug)
        .any(|(at, _)| !text[..at].ends_with(part) && !text[at + slug.len()..].starts_with(part))
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
    /// For a proven commit, what the receipt sykli accepted says of its run:
    /// how many tasks it records, and how many it reused from the cache.
    pub proof: Option<sessions::Evidence>,
    /// For a proven commit, the contract that receipt ran under: the gate
    /// derived from the commit, or its own `sykli.json`.
    pub under: Option<Under>,
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
    /// When `origin/<branch>` last moved in this clone, and how.
    pub updated: Option<Updated>,
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

/// A stable code for why a commit has its mark, for tools to read.
pub fn code(check: &Check) -> &'static str {
    match check {
        Check::Passed => "proven",
        Check::Failed { .. } => "failed",
        Check::OtherContract { .. } => "other_contract",
        Check::Unchecked => "no_receipt",
        Check::UncheckedGone => "no_receipt_checkout_gone",
        Check::Unverified { .. } => "could_not_say",
        Check::Stale { .. } => "stale",
    }
}

/// Why a commit has its mark, for a person; `None` when it is proven.
pub fn why(check: &Check) -> Option<String> {
    Some(match check {
        Check::Passed => return None,
        Check::Failed { why } => why.clone(),
        Check::OtherContract { why } => {
            format!("passed under another contract than the gate's ({why})")
        }
        Check::Unchecked => "no receipt of its tree here".to_owned(),
        Check::UncheckedGone => {
            "no receipt of its tree here, and the checkout it was made in is gone".to_owned()
        }
        Check::Unverified { why } => format!("sykli could not say: {why}"),
        Check::Stale { why } => format!("judged on a checkout, not on the commit: {why}"),
    })
}

/// When `origin/<branch>` last moved in a clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Updated {
    /// Milliseconds.
    pub ms: i64,
    /// `push` or `fetch`, when its reflog says which.
    pub by: Option<&'static str>,
}

/// When `origin/<branch>` last moved in the clone whose common git dir is
/// `common`: the last line of its reflog (a push moves it as well as a
/// fetch, and says so), else the time of its file. Files only.
pub fn updated(common: &Path, branch: &str) -> Option<Updated> {
    let reference = Path::new("refs/remotes/origin").join(branch);
    let logged = std::fs::read_to_string(common.join("logs").join(&reference))
        .ok()
        .and_then(|log| {
            let last = log.lines().rev().find(|line| !line.trim().is_empty())?;
            let (meta, message) = last.split_once('\t').unwrap_or((last, ""));
            let mut fields = meta.split_whitespace().rev();
            let _zone = fields.next()?;
            let seconds: i64 = fields.next()?.parse().ok()?;
            let by = if message.starts_with("update by push") {
                Some("push")
            } else if message.starts_with("fetch") || message.starts_with("pull") {
                Some("fetch")
            } else {
                None
            };
            Some(Updated {
                ms: seconds * 1000,
                by,
            })
        });
    logged.or_else(|| {
        let modified = std::fs::metadata(common.join(&reference))
            .ok()?
            .modified()
            .ok()?;
        let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
        Some(Updated {
            ms: i64::try_from(since.as_millis()).ok()?,
            by: None,
        })
    })
}

/// The marks on `gate`'s branch, against `receipts`: one git process (the
/// clone's `git cat-file --batch`, which gives each commit, first parent by
/// first parent, and the contract of each commit sykli is asked about), and
/// `sykli verify` only for a commit with a receipt of its tree that says it
/// passed, while `sykli` may still verify.
pub fn walk(gate: &Gate, sykli: &Sykli, receipts: &Receipts) -> Result<Marks, String> {
    let mut name = format!("refs/remotes/origin/{}", gate.branch);
    let mut commits = Vec::new();
    let mut ended = None;
    while commits.len() < MAX_WALKED {
        let read = sykli.commit_of(gate, &name);
        let commit = match read {
            Ok(Some(commit)) => commit,
            // Its parent is not here: a shallow clone ends there.
            Ok(None) if !commits.is_empty() => {
                ended = Some(Ended::Root);
                break;
            }
            Ok(None) => {
                return Err(format!(
                    "origin/{} cannot be read in {} (this clone has no {name})",
                    gate.branch,
                    gate.top.display()
                ))
            }
            Err(why) => {
                return Err(format!(
                    "origin/{} cannot be read in {} ({why})",
                    gate.branch,
                    gate.top.display()
                ))
            }
        };
        let asks = receipts
            .of(&commit.tree)
            .iter()
            .any(|receipt| matches!(receipt.evidence.outcome.as_str(), "passed" | "cached"));
        if asks && !sykli.may_verify() {
            ended = Some(Ended::NotAsked);
            break;
        }
        let check = sykli.check_gated(gate, &commit.oid, &commit.tree, receipts, true);
        let proven = check == Check::Passed;
        let proof = proven.then(|| sykli.proof(&commit.oid)).flatten();
        let under = proven.then(|| sykli.proved_under(&commit.oid)).flatten();
        let parent = commit.parent.clone();
        commits.push(Walked {
            commit: commit.oid,
            tree: commit.tree,
            author: commit.author,
            authored: commit.authored,
            subject: commit.subject,
            check,
            proof,
            under,
        });
        if proven {
            ended = Some(Ended::Proven);
            break;
        }
        match parent {
            Some(parent) => name = parent,
            None => {
                ended = Some(Ended::Root);
                break;
            }
        }
    }
    let ended = ended.unwrap_or(Ended::Cap);
    Ok(Marks {
        gate: gate.clone(),
        updated: updated(&gate.common, &gate.branch),
        commits,
        ended,
    })
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// `origin/main as of a push at <time>`: what the marks were read from.
fn as_of(marks: &Marks) -> String {
    let reference = format!("origin/{}", marks.gate.branch);
    match &marks.updated {
        Some(Updated { ms, by: Some(by) }) => {
            format!("{reference} as of a {by} at {}", sessions::clock(*ms))
        }
        Some(Updated { ms, by: None }) => format!("{reference} as of {}", sessions::clock(*ms)),
        None => format!("{reference}, its update time unknown"),
    }
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
    // Runs that passed under another contract are not proven against the
    // gate as it is now: said so, never as if nothing had ever passed.
    let other = unproven
        .iter()
        .filter(|walked| matches!(walked.check, Check::OtherContract { .. }))
        .count();
    let oldest = short(&oldest.commit);
    let since = match (marks.ended, other > 0) {
        (Ended::Proven, false) => format!("is unproven since {oldest}"),
        (Ended::Proven, true) => format!("is not proven against its current gate since {oldest}"),
        (Ended::Root, false) => "has never been proven".to_owned(),
        (Ended::Root, true) => "has never been proven against its current gate".to_owned(),
        (Ended::Cap, false) => format!("is unproven in each of its last {n} commits"),
        (Ended::Cap, true) => {
            format!("is not proven against its current gate in any of its last {n} commits")
        }
        (Ended::NotAsked, false) => format!("is unproven since at least {oldest}"),
        (Ended::NotAsked, true) => {
            format!("is not proven against its current gate since at least {oldest}")
        }
    };
    let commits = if n == 1 { "commit" } else { "commits" };
    let count = if marks.ended == Ended::Cap {
        String::new()
    } else {
        format!("{n} {commits}: ")
    };
    let passed = if other > 0 {
        format!("; {other} passed under another contract")
    } else {
        String::new()
    };
    Some(format!(
        "FLAG: {branch} of {} {since} ({count}{}{passed}; {}). Start by running `ohjaa gate` \
         on {branch} and fix forward what fails.",
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
        let _ = write!(
            out,
            " {} {}{}",
            symbol(&walked.check),
            short(&walked.commit),
            // A proof that reused sykli's cache is said as one.
            walked
                .proof
                .as_ref()
                .map(sessions::describe_cached)
                .unwrap_or_default()
        );
        // Proven by a run of the repository's own contract, not its gate.
        if walked.under == Some(Under::Own) {
            out.push_str(" (own sykli.json)");
        }
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
/// clones of one entry, the walk is in the one updated last, against
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
            let last = |gate: &&&Gate| updated(&gate.common, &gate.branch).map(|at| at.ms);
            let marked = match mine.iter().max_by_key(last) {
                Some(gate) => {
                    let mut receipts = Receipts::default();
                    for clone in &mine {
                        receipts.extend(lifecycle::receipts_of_layout(&clone.common));
                    }
                    walk(gate, sykli, &receipts)
                }
                None => Err(match home::read_entry(&dir) {
                    Ok(_) => "no clone under the roots has it as origin".to_owned(),
                    Err(error) => format!("its entry cannot be read: {error:#}"),
                }),
            };
            (slug, marked)
        })
        .collect()
}

/// What a launch in a repository learns of its entry's branch.
#[derive(Debug, Default)]
pub struct AtLaunch {
    /// The drift warnings doctor would give the clone.
    pub warnings: Vec<String>,
    /// The flag, when the entry's branch is not proven at its tip.
    pub flag: Option<String>,
    /// The marks themselves, when the walk could be made.
    pub marks: Option<Marks>,
    /// The sykli the walk asked, with the clone's one git process still
    /// open: the integrated view reads the gates of the tip and of HEAD
    /// through it, starting none.
    pub sykli: Sykli,
}

/// For `ohjaa run` and `ohjaa brief` in the repository at `top`: the
/// drift warnings doctor would give it, and the flag when the entry's branch
/// is not proven at its tip. Nothing at all without an entry; else one git
/// process for the clone's config, one for the walk (and every commit or
/// gate read after it, through [`AtLaunch::sykli`]), and `sykli verify` at
/// most once.
pub fn at_launch(top: &Path) -> AtLaunch {
    let Ok(home) = home::ohjaa_home() else {
        return AtLaunch::default();
    };
    let home = home::absolute(home);
    let (warnings, gate) = gitconfig::launch_view(&home, top);
    let Some(gate) = gate else {
        return AtLaunch {
            warnings,
            ..AtLaunch::default()
        };
    };
    let sykli = Sykli::with_home(home);
    sykli.allow_verifies(1);
    let receipts = lifecycle::receipts_of_layout(&gate.common);
    let marks = walk(&gate, &sykli, &receipts).ok();
    AtLaunch {
        warnings,
        flag: marks.as_ref().and_then(flag),
        marks,
        sykli,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(checks: &[Check], ended: Ended) -> Marks {
        Marks {
            gate: Gate {
                slug: "acme/widget".to_owned(),
                branch: "main".to_owned(),
                entry: PathBuf::from("/h"),
                overlay: crate::derived::Overlay::standard(),
                top: PathBuf::from("/r"),
                common: PathBuf::from("/r/.git"),
            },
            updated: None,
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
                    proof: None,
                    under: None,
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
             1111111; origin/main, its update time unknown). Start by running `ohjaa gate` on \
             main and fix forward what fails."
        );
        assert_eq!(
            line(&proven),
            "acme/widget main (origin/main, its update time unknown): ? 0000000 ✗ 1111111 ✓ \
             2222222"
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
            ) && capped.contains(", …; origin/main, its update time unknown)"),
            "{capped}"
        );
        let not_asked = flag(&marks(&[Check::Unchecked], Ended::NotAsked)).unwrap();
        assert!(
            not_asked.contains("is unproven since at least 0000000 (1 commit: "),
            "{not_asked}"
        );

        // B1: a run that passed under another contract is not proven
        // against the gate as it is now, and it is said so.
        let other = Check::OtherContract {
            why: "refused: contract expected a but got b".to_owned(),
        };
        let regated = marks(&[Check::Unchecked, other.clone()], Ended::Root);
        assert_eq!(
            flag(&regated).unwrap(),
            "FLAG: main of acme/widget has never been proven against its current gate (2 \
             commits: ? 0000000, ? 1111111; 1 ran under another contract; origin/main, its \
             update time unknown). Start by running `ohjaa gate` on main and fix forward what \
             fails."
        );
        let since = flag(&marks(&[other.clone(), Check::Passed], Ended::Proven)).unwrap();
        assert!(
            since.starts_with(
                "FLAG: main of acme/widget is not proven against its current gate since 0000000"
            ),
            "{since}"
        );
        assert_eq!(symbol(&other), "?");
        assert_eq!(word(&other), "unproven");
        assert_eq!(code(&other), "other_contract");
        assert_eq!(
            why(&other).unwrap(),
            "ran under another contract than the gate's (refused: contract expected a but \
             got b)"
        );
        assert_eq!(why(&Check::Passed), None);
    }

    #[test]
    fn a_config_names_an_entry_only_as_a_whole() {
        let config = "[remote \"origin\"]\n\turl = git@github.com:acme/r1.git\n";
        assert!(names(config, "acme/r1"));
        assert!(names("url = /srv/acme/r1\n", "acme/r1"));
        assert!(!names("url = git@github.com:acme/r10.git\n", "acme/r1"));
        assert!(!names("url = git@github.com:xacme/r1.git\n", "acme/r1"));
    }

    /// When origin's branch last moved, from its reflog: a push moves it as
    /// well as a fetch, and says which; else its file's time.
    #[test]
    fn the_branch_is_read_as_of_its_last_update() {
        let common = std::env::temp_dir().join(format!("ohjaa-updated-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&common);
        let log = common.join("logs/refs/remotes/origin");
        std::fs::create_dir_all(&log).unwrap();
        let zeros = "0".repeat(40);
        std::fs::write(
            log.join("main"),
            format!(
                "{zeros} {zeros} A <a@x> 1700000000 +0200\tfetch: fast-forward\n\
                 {zeros} {zeros} A <a@x> 1700000060 +0200\tupdate by push\n"
            ),
        )
        .unwrap();
        assert_eq!(
            updated(&common, "main"),
            Some(Updated {
                ms: 1_700_000_060_000,
                by: Some("push")
            })
        );
        std::fs::write(
            log.join("main"),
            format!("{zeros} {zeros} A <a@x> 1700000000 +0200\tfetch origin: fast-forward\n"),
        )
        .unwrap();
        assert_eq!(updated(&common, "main").unwrap().by, Some("fetch"));
        // No reflog: the ref's own file.
        std::fs::create_dir_all(common.join("refs/remotes/origin")).unwrap();
        std::fs::write(common.join("refs/remotes/origin/trunk"), "x\n").unwrap();
        let file = updated(&common, "trunk").unwrap();
        assert_eq!(file.by, None);
        assert!(file.ms > 1_700_000_000_000);
        assert_eq!(updated(&common, "gone"), None);
        std::fs::remove_dir_all(&common).unwrap();
    }
}
