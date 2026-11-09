//! What sykli says of commits: the gate a checkout is judged by, its runs
//! and receipts, and the commits a default branch gained in a window, each
//! with the evidence that it passed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::process;
use crate::sessions::{Check, Evidence, ReceiptRef};

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = process::run(
        Command::new("git").arg("-C").arg(repo).args(args),
        None,
        process::LOCAL,
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| output.stdout_text().trim().to_owned())
}

pub fn head(repo: &Path) -> Option<String> {
    git(repo, &["rev-parse", "--verify", "-q", "HEAD"])
}

/// The Git repository `path` lies in, canonical. A path not created yet (a
/// file the agent will write) is looked up from its nearest existing
/// directory. `None` when that directory is not inside a repository.
pub fn repo_of(path: &Path) -> Option<std::path::PathBuf> {
    let dir = path.ancestors().find(|dir| dir.is_dir())?;
    let root = git(dir, &["rev-parse", "--show-toplevel"])?;
    std::fs::canonicalize(root).ok()
}

/// Where ohjaa's own validation checkouts are (see `integrated`), canonical:
/// it makes nothing in them, so their reflogs are never read for work (their
/// receipts still are).
pub fn validation_checkouts() -> Option<PathBuf> {
    let state = crate::knowledge::state_home().ok()?;
    Some(canonical(&crate::integrated::checkouts_root(
        &state.join("ohjaa"),
    )))
}

/// One checkout of a repository: its main worktree or a linked one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Canonical.
    pub path: PathBuf,
    pub head: Option<String>,
    /// `refs/heads/<name>`, when a branch is checked out there.
    pub branch: Option<String>,
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every checkout of the repository `repo` lies in, the main one first, as
/// `git worktree list --porcelain` names them. A bare main repository and
/// checkouts whose folder is gone are left out. Only `repo` itself when git
/// cannot list them.
pub fn worktrees(repo: &Path) -> Vec<Worktree> {
    let listed = git(repo, &["worktree", "list", "--porcelain"])
        .map(|porcelain| parse_worktrees(&porcelain))
        .unwrap_or_default();
    if listed.is_empty() {
        return vec![Worktree {
            path: canonical(repo),
            head: head(repo),
            branch: git(repo, &["symbolic-ref", "-q", "HEAD"]),
        }];
    }
    listed
}

fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    porcelain
        .split("\n\n")
        .filter_map(|block| {
            let mut path = None;
            let mut head = None;
            let mut branch = None;
            for line in block.lines() {
                match line.split_once(' ') {
                    Some(("worktree", value)) => path = Some(PathBuf::from(value)),
                    Some(("HEAD", value)) => head = Some(value.to_owned()),
                    Some(("branch", value)) => branch = Some(value.to_owned()),
                    _ if line == "bare" => return None,
                    _ => {}
                }
            }
            let path = path.filter(|path| path.is_dir())?;
            Some(Worktree {
                path: canonical(&path),
                head,
                branch,
            })
        })
        .collect()
}

/// A sykli receipt: what it says, and where it lies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    /// When the run started, in milliseconds.
    pub at: i64,
    pub evidence: Evidence,
    pub path: PathBuf,
    /// The checkout sykli ran in.
    pub checkout: PathBuf,
    /// The hash of the contract it ran under, and the digest of the lock
    /// beside it, as the receipt names them: which contract to verify it
    /// against.
    pub contract_hash: Option<String>,
    pub lock_digest: Option<String>,
}

/// Sykli receipts keyed by the tree that ran: every one of each tree, best
/// first (a passing run, then a clean one, then the newest). A newer run of
/// the same tree, under another contract or failing, never hides a pass.
/// Beside them, what could not be read of the receipts sykli named by
/// their digest: said, never taken for a receipt.
#[derive(Debug, Clone, Default)]
pub struct Receipts {
    by_tree: std::collections::HashMap<String, Vec<Receipt>>,
    /// Each a path, and why it cannot be read.
    unreadable: Vec<String>,
}

impl Receipts {
    /// The best receipt of `tree`.
    #[cfg(test)]
    pub fn get(&self, tree: &str) -> Option<&Receipt> {
        self.by_tree.get(tree).and_then(|all| all.first())
    }

    /// Every receipt of `tree`, best first.
    pub fn of(&self, tree: &str) -> &[Receipt] {
        self.by_tree.get(tree).map_or(&[], Vec::as_slice)
    }

    pub fn is_empty(&self) -> bool {
        self.by_tree.is_empty()
    }

    /// The receipts sykli named by their digest (`rcpt_<sha256>.json`) that
    /// could not be read, each a path and why: a gap, never a receipt.
    pub fn unreadable(&self) -> &[String] {
        &self.unreadable
    }

    /// Every receipt of `other` too.
    pub fn extend(&mut self, other: Receipts) {
        for (tree, all) in other.by_tree {
            for receipt in all {
                self.add(&tree, receipt);
            }
        }
        self.unreadable.extend(other.unreadable);
    }

    fn add(&mut self, tree: &str, receipt: Receipt) {
        let rank = |r: &Receipt| (r.evidence.outcome == "passed", !r.evidence.dirty, r.at);
        let all = self.by_tree.entry(tree.to_owned()).or_default();
        let at = all
            .iter()
            .position(|seen| rank(seen) < rank(&receipt))
            .unwrap_or(all.len());
        all.insert(at, receipt);
    }
}

#[cfg(test)]
impl std::ops::Index<&str> for Receipts {
    type Output = Receipt;

    fn index(&self, tree: &str) -> &Receipt {
        self.get(tree).expect("a receipt of that tree")
    }
}

/// The sykli receipts in these checkouts' `.sykli/receipts` (see
/// [`receipts_from`]).
#[cfg(test)]
pub fn receipts_in<'a>(checkouts: impl IntoIterator<Item = &'a Path>) -> Receipts {
    receipts_from(
        checkouts
            .into_iter()
            .map(|checkout| (checkout.join(".sykli").join("receipts"), checkout)),
    )
}

/// The receipts in every checkout of `repo`'s clone, and those sykli keeps
/// under the git common dir ([`kept_by_sykli`]), which outlive a worktree
/// removed with its `.sykli/`. Those are run from `repo`.
pub fn receipts_of_clone(repo: &Path, checkouts: &[Worktree]) -> Receipts {
    let kept = crate::gitconfig::common_dir(repo)
        .ok()
        .map(|common| kept_by_sykli(&common))
        .unwrap_or_default();
    receipts_from(
        checkouts
            .iter()
            .flat_map(|w| {
                [
                    (w.path.join(".sykli").join("receipts"), w.path.as_path()),
                    (w.path.join(".sykli").join("receipt.json"), w.path.as_path()),
                ]
            })
            .chain(kept.into_iter().map(|dir| (dir, repo))),
    )
}

/// Where sykli keeps receipts under a clone's common git dir, so they
/// outlive the worktree that ran them: `<common>/sykli/receipts` (the
/// receipt that verified each push its hook accepted) and the folder of each
/// run it keeps, `<common>/sykli/runs/<run>/`, which holds the receipt the
/// run concluded with (`rcpt_<sha256>.json`; `sykli status --run` names it,
/// `verify --commit` judges it from any worktree of the clone). sykli keeps
/// at most `SYKLI_RUN_BUDGET` runs per clone (100 by default), the oldest
/// removed first. Files only.
fn kept_by_sykli(common: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![common.join("sykli").join("receipts")];
    if let Ok(runs) = std::fs::read_dir(common.join("sykli").join("runs")) {
        let mut runs: Vec<PathBuf> = runs
            .flatten()
            .filter(|run| {
                // A run's id is a digest; sykli's own leftovers start with a dot.
                run.file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with('.'))
            })
            .map(|run| run.path())
            .collect();
        runs.sort();
        dirs.extend(runs);
    }
    dirs
}

/// The receipts of a clone found from its layout alone, without asking git:
/// those sykli keeps under the common dir ([`kept_by_sykli`]: its pre-push
/// hook's, and each run's, which outlive a worktree) and, in every worktree
/// (the main one, and each `<common>/worktrees/*/gitdir` names), the ones
/// `sykli run` writes (`.sykli/receipts/`) and the one `ohjaa gate` saves
/// (`.sykli/receipt.json`).
pub fn receipts_of_layout(common: &Path) -> Receipts {
    let checkouts = layout_checkouts(common);
    let kept_in = checkouts
        .first()
        .cloned()
        .unwrap_or_else(|| common.to_path_buf());
    let mut dirs: Vec<(PathBuf, PathBuf)> = checkouts
        .iter()
        .flat_map(|checkout| {
            [
                (checkout.join(".sykli").join("receipts"), checkout.clone()),
                (
                    checkout.join(".sykli").join("receipt.json"),
                    checkout.clone(),
                ),
            ]
        })
        .collect();
    dirs.extend(
        kept_by_sykli(common)
            .into_iter()
            .map(|dir| (dir, kept_in.clone())),
    );
    receipts_from(
        dirs.iter()
            .map(|(dir, checkout)| (dir.clone(), checkout.as_path())),
    )
}

/// Every checkout of the clone whose common git dir is `common`, canonical,
/// from its layout alone (no git): the main one when the common dir is its
/// `.git`, then each linked one `<common>/worktrees/*/gitdir` names, whether
/// or not its folder still exists.
pub fn layout_checkouts(common: &Path) -> Vec<PathBuf> {
    let mut checkouts: Vec<PathBuf> = Vec::new();
    if common.file_name().is_some_and(|name| name == ".git") {
        if let Some(main) = common.parent() {
            checkouts.push(canonical(main));
        }
    }
    if let Ok(linked) = std::fs::read_dir(common.join("worktrees")) {
        let mut linked: Vec<PathBuf> = linked.flatten().map(|entry| entry.path()).collect();
        linked.sort();
        for admin in linked {
            let Ok(gitdir) = std::fs::read_to_string(admin.join("gitdir")) else {
                continue;
            };
            let gitdir = admin.join(gitdir.trim());
            if let Some(worktree) = gitdir.parent() {
                checkouts.push(canonical(worktree));
            }
        }
    }
    checkouts
}

/// The sykli receipts in these folders, each with the checkout it is run
/// from, keyed by the tree that ran. Reads `sykli-receipt.v1`:
/// `subject.tree_oid`, `subject.head_tree_oid`, `subject.dirty`, `outcome`,
/// `started_at_ms`. A tree is its content, so a receipt is about that tree in
/// whichever checkout sykli ran. Every receipt of a tree is kept, best first: a
/// passing run, then a clean one, then the newest. Read once per repository at leaving,
/// and shared by its phases and its pull request.
///
/// sykli names a receipt it keeps by the SHA-256 of its bytes
/// (`rcpt_<sha256>.json`), so one kept in several places (a worktree's
/// `.sykli/receipts/` and its run's folder) is the same receipt, read once,
/// where it is first found, and never asked about twice. Only the same
/// bytes are the same receipt: another file under that name (another
/// receipt, or one cut short) never hides it. A file so named that cannot
/// be read as a receipt is said ([`Receipts::unreadable`]), never taken for
/// one; one gone between the listing and the reading (sykli pruned it) is
/// no receipt at all.
fn receipts_from<'a>(dirs: impl IntoIterator<Item = (PathBuf, &'a Path)>) -> Receipts {
    let mut found = Receipts::default();
    // Every path a receipt was read from, by the name sykli gave it.
    let mut named_at: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for (dir, checkout) in dirs {
        // A folder of receipts, or one receipt file (`.sykli/receipt.json`).
        let paths: Vec<PathBuf> = if dir.is_file() {
            vec![dir]
        } else {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            entries.flatten().map(|entry| entry.path()).collect()
        };
        for path in paths {
            if path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let named = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix("rcpt_")?.strip_suffix(".json"))
                .filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
                .map(str::to_owned);
            let unreadable = |found: &mut Receipts, why: String| {
                if named.is_some() {
                    found.unreadable.push(format!("{} ({why})", path.display()));
                }
            };
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    unreadable(&mut found, error.to_string());
                    continue;
                }
            };
            let same = named
                .as_ref()
                .and_then(|name| named_at.get(name))
                .is_some_and(|read| {
                    read.iter()
                        .any(|seen| std::fs::read(seen).is_ok_and(|seen| seen == bytes))
                });
            if same {
                continue;
            }
            let receipt = match serde_json::from_slice::<Value>(&bytes) {
                Ok(receipt) => receipt,
                Err(error) => {
                    unreadable(&mut found, format!("not JSON: {error}"));
                    continue;
                }
            };
            if receipt["schema"] != "sykli-receipt.v1" {
                continue;
            }
            // Filed under the tree that ran, never HEAD's: a run with changes
            // beside HEAD ran another tree, and must not shadow HEAD's own run.
            let (Some(tree), Some(outcome)) = (
                receipt["subject"]["tree_oid"].as_str(),
                receipt["outcome"].as_str(),
            ) else {
                unreadable(
                    &mut found,
                    "a sykli-receipt.v1 that names no tree or no outcome".to_owned(),
                );
                continue;
            };
            if let Some(name) = &named {
                named_at.entry(name.clone()).or_default().push(path.clone());
            }
            let at = receipt["started_at_ms"].as_i64().unwrap_or(0);
            let head = receipt["subject"]["head_tree_oid"].as_str();
            // sykli calls a run dirty when the tree that ran is not HEAD's, or
            // something beside it (a modified submodule) is. A receipt is filed
            // under the tree that ran, so a run made before the commit (sykli's
            // pre-commit hook, HEAD still at the parent) is that tree's run all
            // the same: only something beside the tree is dirty here.
            let dirty = match receipt["subject"]["dirty"].as_bool() {
                Some(false) => false,
                Some(true) => head.is_none_or(|head| head == tree),
                None => true,
            };
            // What each task did: run, or reused from sykli's cache. Counted
            // apart, so a reuse is never shown as a run that passed.
            let tasks = receipt["tasks"].as_array().map_or(&[][..], Vec::as_slice);
            let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            let evidence = Evidence {
                outcome: outcome.to_owned(),
                dirty,
                tasks: count(tasks.len()),
                cached: count(
                    tasks
                        .iter()
                        .filter(|task| task["outcome"] == "cached")
                        .count(),
                ),
            };
            found.add(
                tree,
                Receipt {
                    at,
                    evidence,
                    path,
                    checkout: checkout.to_path_buf(),
                    contract_hash: receipt["contract_hash"].as_str().map(str::to_owned),
                    lock_digest: receipt["lock_digest"].as_str().map(str::to_owned),
                },
            );
        }
    }
    found
}

/// One reflog entry: a reference moved to `commit`.
struct Move {
    commit: String,
    subject: String,
}

const REFLOG_FORMAT: &str = "--format=%gD%x1f%H%x1f%gs";

/// Reflog entries, newest first per reference, from `git <args>` run with
/// `--date=unix` and [`REFLOG_FORMAT`].
fn reflog(dir: &Path, args: &[&str]) -> Option<Vec<Move>> {
    let log = git(dir, args)?;
    Some(
        log.lines()
            .filter_map(|line| {
                let mut fields = line.splitn(3, '\u{1f}');
                let (selector, commit, subject) = (fields.next()?, fields.next()?, fields.next()?);
                // `refs/heads/main@{1790380435}`, with --date=unix.
                let (_, at) = selector.rsplit_once("@{")?;
                at.strip_suffix('}')?.parse::<i64>().ok()?;
                Some(Move {
                    commit: commit.to_owned(),
                    subject: subject.to_owned(),
                })
            })
            .collect(),
    )
}

/// A commit on a repository's default branch, and what sykli says of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDefault {
    /// `origin/main`, else the local `main`.
    pub onto: String,
    pub commit: String,
    pub subject: String,
    /// Its commit time, in unix seconds.
    pub committed: i64,
    /// It has more than one parent: a merge (a forge's "Merge pull request").
    pub merge: bool,
    pub check: Check,
}

/// The most default-branch commits a report reads in one repository.
pub const MAX_REPORTED: usize = 100;

/// The commits on `repo`'s default branch (the remote one, else the local;
/// for a repository with an entry, origin's copy of the entry's branch)
/// committed after `since` (unix seconds), first parent only, newest first,
/// at most [`MAX_REPORTED`] (`true` beside them when the window held more),
/// each with what sykli says of it, against the receipts in every checkout
/// and those kept under the git common dir. `None` when git cannot read it.
pub fn default_branch_since(
    repo: &Path,
    since: i64,
    sykli: &Sykli,
) -> Option<(Vec<OnDefault>, bool)> {
    let repo = canonical(repo);
    // An entry names the branch it observes; else the default branch.
    let reference = match sykli.gate_for(&repo) {
        Some(gate) => format!("refs/remotes/origin/{}", gate.branch),
        None => default_branch(&repo).into_iter().next()?,
    };
    let onto = reference
        .strip_prefix("refs/remotes/")
        .or_else(|| reference.strip_prefix("refs/heads/"))
        .unwrap_or(&reference)
        .to_owned();
    let log = git(
        &repo,
        &[
            "log",
            "--first-parent",
            &format!("--since=@{since}"),
            &format!("-{}", MAX_REPORTED + 1),
            "--format=%H%x1f%T%x1f%ct%x1f%P%x1f%s",
            &reference,
        ],
    )?;
    let all = worktrees(&repo);
    let receipts = receipts_of_clone(&repo, &all);
    // What a checkout that still exists records making: its receipts would
    // be there.
    let mut alive = HashSet::new();
    let ours = validation_checkouts();
    for checkout in &all {
        if ours
            .as_ref()
            .is_some_and(|ours| checkout.path.starts_with(ours))
        {
            continue;
        }
        let moves = reflog(
            &checkout.path,
            &[
                "log",
                "--walk-reflogs",
                "--date=unix",
                REFLOG_FORMAT,
                "HEAD",
            ],
        );
        for entry in moves.into_iter().flatten() {
            if created_here(&entry.subject) {
                alive.insert(entry.commit);
            }
        }
    }
    let mut listed: Vec<(&str, &str, i64, bool, &str)> = log
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(5, '\u{1f}');
            let (commit, tree, committed, parents, subject) = (
                fields.next()?,
                fields.next()?,
                fields.next()?.parse::<i64>().ok()?,
                fields.next()?,
                fields.next()?,
            );
            // `--since` keeps the second it names; the window does not.
            (committed > since).then_some((
                commit,
                tree,
                committed,
                parents.split_whitespace().count() > 1,
                subject,
            ))
        })
        .collect();
    let capped = listed.len() > MAX_REPORTED;
    listed.truncate(MAX_REPORTED);
    let judged = listed
        .into_iter()
        .map(|(commit, tree, committed, merge, subject)| OnDefault {
            onto: onto.clone(),
            commit: commit.to_owned(),
            subject: subject.to_owned(),
            committed,
            merge,
            check: sykli.check(&repo, commit, tree, &receipts, &all, alive.contains(commit)),
        })
        .collect();
    Some((judged, capped))
}

/// A sykli that judges a repository whose origin names an entry in the
/// workflow home by the entry's gate (every other one by its own contract).
pub fn sykli_with_entries() -> Sykli {
    match crate::home::ohjaa_home() {
        Ok(home) => Sykli::with_home(crate::home::absolute(home)),
        Err(_) => Sykli::default(),
    }
}

/// Whether this sykli can bind a receipt to a commit, and why not when it
/// cannot: for a report to say what it could not check.
pub fn sykli_cannot_bind(sykli: &Sykli) -> Option<String> {
    sykli.answer().err()
}

/// The default branch's references: the remote one first, then the local
/// one, each when it exists. The default branch is what `origin/HEAD` names,
/// else `main`, else `master`.
fn default_branch(repo: &Path) -> Vec<String> {
    let exists = |reference: &str| git(repo, &["rev-parse", "--verify", "-q", reference]).is_some();
    let name = git(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
        .and_then(|target| {
            target
                .strip_prefix("refs/remotes/origin/")
                .map(str::to_owned)
        })
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|name| {
                    exists(&format!("refs/remotes/origin/{name}"))
                        || exists(&format!("refs/heads/{name}"))
                })
                .map(str::to_owned)
        });
    let Some(name) = name else {
        return Vec::new();
    };
    [
        format!("refs/remotes/origin/{name}"),
        format!("refs/heads/{name}"),
    ]
    .into_iter()
    .filter(|reference| exists(reference))
    .collect()
}

/// Whether a reflog entry records HEAD arriving at a commit this checkout
/// created, rather than moving to one that already existed (a checkout, a
/// reset, a fast-forward, a rebase's start or finish). Judged by git's own
/// wording, `<action>: <message>`. The action carries the command's
/// arguments (`pull -q --rebase (pick)`), so replays match by their suffix.
fn created_here(subject: &str) -> bool {
    const REPLAYED: [&str; 7] = [
        "(pick)",
        "(reword)",
        "(edit)",
        "(squash)",
        "(fixup)",
        "(merge)",
        "(continue)",
    ];
    let (action, message) = subject.split_once(": ").unwrap_or((subject, ""));
    action.starts_with("commit")
        || ["cherry-pick", "revert", "am"].contains(&action)
        || REPLAYED.iter().any(|step| action.ends_with(step))
        || message.starts_with("Merge made by")
}

/// The most receipts of one tree sykli is asked about, best first.
const MAX_ASKED: usize = 5;

/// An entry's gate for one clone: the entry the workflow home keeps for
/// the repository its origin names, the branch it observes, and the clone
/// (a checkout's top level and the common git dir), which sykli is handed
/// instead of asking git for it again. The contract each commit is judged
/// by is derived from the commit ([`crate::derived::GateAt`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// `owner/name`.
    pub slug: String,
    pub branch: String,
    /// The entry's folder, which names it (the gates derived for its
    /// commits are written outside it, see [`crate::derived::Gates`]).
    pub entry: PathBuf,
    /// What each cargo task of a derived gate is given.
    pub overlay: crate::derived::Overlay,
    pub top: PathBuf,
    pub common: PathBuf,
}

/// How a sykli's `verify --commit` gives its verdict, from the flags
/// `sykli verify --help` lists, never its sentences. Every sykli that has
/// `--commit` judges the commit alone, nothing from the checkout (sykli#67
/// as merged; the one that also compared the checkout lived only on that
/// pull request's branch, and only a sentence told it apart).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// `--json`: a `sykli-verify.v1` document, read by [`proves`].
    Document,
    /// Its exit code alone.
    Exit,
}

/// The tiers of a `sykli-verify.v1` verdict a proof stands on: the
/// receipt's identity, against the commit. Its schema, the contract it ran
/// under, the source (the commit's tree and every declared input the run
/// read) and its records' consistency. Not the outcome: that is the
/// receipt's claim, since receipts are unsigned (sykli establishes no
/// provenance, producer or execution either). It is read as claimed
/// (`claimed_outcome`, or an older sykli's `outcome` check), whether sykli
/// lists it among the tiers it established (sykli main as of 5acdf91) or
/// not (as it is moving to).
const IDENTITY_TIERS: [&str; 4] = ["schema", "contract", "source", "consistency"];

/// Whether what `sykli verify <receipt> --commit <commit> --json` printed,
/// exiting 0, proves `commit` as the receipt claims: a `sykli-verify.v1`
/// document that judged that commit, verified it, established every one of
/// [`IDENTITY_TIERS`], and says the receipt claims `passed` or `cached`.
/// Else why not, for "sykli could not say".
fn proves(document: &[u8], commit: &str) -> Result<(), String> {
    let document: Value = serde_json::from_slice(document)
        .map_err(|error| format!("sykli verify --json printed no verdict ({error})"))?;
    let schema = document["schema"].as_str().unwrap_or("no schema");
    if schema != "sykli-verify.v1" {
        return Err(format!(
            "sykli verify answered in {schema}, which this ohjaa does not read; update ohjaa"
        ));
    }
    let subject = &document["subject"];
    if subject["kind"] != "commit" || subject["rev"].as_str().is_some_and(|rev| rev != commit) {
        return Err(format!(
            "sykli verify judged {subject}, not the commit {commit}"
        ));
    }
    if document["verified"] != true {
        return Err("sykli verify exited 0 without saying verified".to_owned());
    }
    let established: Vec<&str> = document["trust"]["established"]
        .as_array()
        .map(|tiers| tiers.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = IDENTITY_TIERS
        .into_iter()
        .filter(|tier| !established.contains(tier))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "sykli verified it without establishing its {}",
            missing.join(", ")
        ));
    }
    // The receipt's claim is sykli's machine field `claimed_outcome`. Only
    // a sykli without it (5acdf91 and before) has it read from the words of
    // its `outcome` check (`"actual":"passed with 2 task records"`), the one
    // place that sykli says it, and only where that check passed.
    if let Some(claimed) = document["claimed_outcome"].as_str() {
        return match claimed {
            "passed" | "cached" => Ok(()),
            _ => Err(format!("the receipt claims {claimed}, not a pass")),
        };
    }
    let Some(outcome) = document["checks"]
        .as_array()
        .and_then(|checks| checks.iter().find(|check| check["name"] == "outcome"))
    else {
        return Err("sykli's verdict names no outcome the receipt claims".to_owned());
    };
    let claimed = outcome["actual"].as_str().unwrap_or("nothing");
    let a_pass = claimed
        .split_whitespace()
        .any(|word| matches!(word, "passed" | "cached"));
    if outcome["ok"] == true && a_pass {
        Ok(())
    } else {
        Err(format!("the receipt claims {claimed}, not a pass"))
    }
}

/// This machine's sykli, asked at most once per leaving how it binds a
/// receipt to a commit (`sykli verify --commit`).
///
/// Made [`Sykli::with_home`], a repository whose origin names an entry in
/// that workflow home is judged by the entry's gate, not by a contract in the
/// commit; a commit judged once against a gate is not asked about again.
#[derive(Debug, Default)]
pub struct Sykli {
    /// How `verify --commit` answers; `Err` when this sykli has none.
    answer: std::cell::OnceCell<Result<Answer, String>>,
    /// `sykli verify --help` lists `--repo-root` and `--git-common-dir`.
    takes_facts: std::cell::OnceCell<bool>,
    /// Where entries are looked up; `None` judges every repository by its
    /// own contract.
    home: Option<PathBuf>,
    /// Each repository's gate, by its canonical path, or why a clone that
    /// may be an entry's could not say.
    gates: std::cell::RefCell<HashMap<PathBuf, Result<Option<Gate>, crate::mark::Unasked>>>,
    /// What a gate said of a commit: (entry, commit) to the verdict.
    judged: std::cell::RefCell<HashMap<(PathBuf, String), Check>>,
    /// One `git cat-file --batch` per clone, by its common dir: every
    /// commit and file this ohjaa reads of it, from one git process.
    objects: std::cell::RefCell<HashMap<PathBuf, crate::derived::Objects>>,
    /// The gate of each commit: (entry, commit) to it.
    gate_at: std::cell::RefCell<HashMap<(PathBuf, String), crate::derived::GateAt>>,
    /// Which contract the receipt that proved each commit ran under.
    proved_under: std::cell::RefCell<HashMap<String, crate::derived::Under>>,
    /// Where the gates sykli reads are written: never the config home.
    derived: crate::derived::Gates,
    /// How many times `sykli verify` ran.
    verified: std::cell::Cell<usize>,
    /// How many times it may run in all; `None`: as often as it is needed.
    allowance: std::cell::Cell<Option<usize>>,
    /// What the receipt sykli verified for each commit said of its run: how
    /// many tasks it ran, and how many it reused from the cache.
    proofs: std::cell::RefCell<HashMap<String, Evidence>>,
    /// The sykli to run, when not the one on PATH: the one `ohjaa apply`
    /// recorded in a dispatcher.
    program: Option<PathBuf>,
    /// How long one sykli call may take; `None`: [`process::LOCAL`].
    limit: Option<std::time::Duration>,
}

impl Sykli {
    /// A sykli that judges a repository with an entry in `home` by its gate.
    pub fn with_home(home: PathBuf) -> Sykli {
        Sykli {
            home: Some(home),
            derived: crate::derived::Gates::of_state(),
            ..Sykli::default()
        }
    }

    /// The same, running `program` for sykli, and each call for at most
    /// `limit`: a git hook's, which a person waits on.
    pub fn with_program(home: PathBuf, program: PathBuf, limit: std::time::Duration) -> Sykli {
        Sykli {
            home: Some(home),
            program: Some(program),
            limit: Some(limit),
            derived: crate::derived::Gates::of_state(),
            ..Sykli::default()
        }
    }

    fn command(&self) -> Command {
        Command::new(self.program.as_deref().unwrap_or(Path::new("sykli")))
    }

    fn timeout(&self) -> std::time::Duration {
        self.limit.unwrap_or(process::LOCAL)
    }

    fn proved(&self, commit: &str, receipt: &Receipt, under: crate::derived::Under) {
        self.proofs
            .borrow_mut()
            .insert(commit.to_owned(), receipt.evidence.clone());
        self.proved_under
            .borrow_mut()
            .insert(commit.to_owned(), under);
    }

    /// Which contract proved `commit`, once sykli verified a receipt of it
    /// here: the gate derived from it, or its own `sykli.json`.
    pub fn proved_under(&self, commit: &str) -> Option<crate::derived::Under> {
        self.proved_under.borrow().get(commit).copied()
    }

    /// Ask the clone whose common git dir is `common`, through its one `git
    /// cat-file --batch` (started the first time it is asked).
    pub fn ask_objects<T>(
        &self,
        common: &Path,
        ask: impl FnOnce(&mut crate::derived::Objects) -> Result<T, String>,
    ) -> Result<T, String> {
        let key = canonical(common);
        let mut all = self.objects.borrow_mut();
        let objects = match all.entry(key) {
            std::collections::hash_map::Entry::Occupied(found) => found.into_mut(),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(crate::derived::Objects::of_clone(common)?)
            }
        };
        ask(objects)
    }

    /// Where this sykli's gates are written for sykli to read.
    pub fn gates(&self) -> &crate::derived::Gates {
        &self.derived
    }

    /// The commit `name` names in `gate`'s clone.
    pub fn commit_of(
        &self,
        gate: &Gate,
        name: &str,
    ) -> Result<Option<crate::derived::Commit>, String> {
        self.ask_objects(&gate.common, |objects| objects.commit(name))
    }

    /// The gate `commit` is judged by, derived from the commit: from its own
    /// `sykli.json`, else from what `sykli init` writes for its tree (as a
    /// run kept it, else asked of this sykli in a scratch checkout of the
    /// tree, two git processes more); read from the commit once, in the
    /// clone's one git process.
    pub fn gate_at(&self, gate: &Gate, commit: &str) -> Result<crate::derived::GateAt, String> {
        let key = (gate.entry.clone(), commit.to_owned());
        if let Some(known) = self.gate_at.borrow().get(&key) {
            return Ok(known.clone());
        }
        let holds = self.ask_objects(&gate.common, |objects| objects.holds_at(commit))?;
        let program = self.program.as_deref().unwrap_or(Path::new("sykli"));
        let at = crate::derived::GateAt::of(&gate.overlay, commit, holds, |tree| {
            self.derived
                .inferred(&gate.common, tree, program, self.timeout())
        });
        self.gate_at.borrow_mut().insert(key, at.clone());
        Ok(at)
    }

    /// What the receipt that proved `commit` says of its run, once sykli
    /// verified one here: so a reuse of the cache is said as such.
    pub fn proof(&self, commit: &str) -> Option<Evidence> {
        self.proofs.borrow().get(commit).cloned()
    }

    /// Let `sykli verify` run at most `times` more times.
    pub fn allow_verifies(&self, times: usize) {
        self.allowance.set(Some(self.verified.get() + times));
    }

    /// Whether `sykli verify` may still run.
    pub fn may_verify(&self) -> bool {
        self.allowance
            .get()
            .is_none_or(|allowed| self.verified.get() < allowed)
    }

    /// The gate an entry gives `repo`: one git process (its config) the
    /// first time it is asked, none when the home has no entries.
    pub fn gate_for(&self, repo: &Path) -> Option<Gate> {
        self.gate_asked(repo).ok().flatten()
    }

    /// [`Sykli::gate_for`], with why a clone that may be an entry's could
    /// not say ([`crate::mark::gate_asked`]).
    pub fn gate_asked(&self, repo: &Path) -> Result<Option<Gate>, crate::mark::Unasked> {
        let Some(home) = self.home.as_deref() else {
            return Ok(None);
        };
        let key = canonical(repo);
        if let Some(known) = self.gates.borrow().get(&key) {
            return known.clone();
        }
        let gate = crate::mark::gate_asked(home, &key);
        self.gates.borrow_mut().insert(key, gate.clone());
        gate
    }

    /// Ask sykli once, by the flags `sykli verify --help` lists, how it
    /// binds a receipt to a commit, and whether it takes the repository's
    /// facts.
    fn answer(&self) -> Result<Answer, String> {
        self.answer
            .get_or_init(|| {
                let output = process::run(
                    self.command().args(["verify", "--help"]),
                    None,
                    self.timeout(),
                )
                .map_err(|error| format!("sykli could not be asked: {error:#}"))?;
                let help = format!(
                    "{}{}",
                    output.stdout_text(),
                    String::from_utf8_lossy(&output.stderr)
                );
                let names = |flag| process::names_flag(&help, flag);
                let _ = self
                    .takes_facts
                    .set(names("--repo-root") && names("--git-common-dir"));
                match (names("--commit"), names("--json")) {
                    (false, _) => Err("sykli has no --commit".to_owned()),
                    (true, true) => Ok(Answer::Document),
                    (true, false) => Ok(Answer::Exit),
                }
            })
            .clone()
    }

    /// One `sykli verify <receipt> … --commit <commit>` (`command`, all but
    /// `--json`): `Passed`, with the proof kept, when sykli proves `commit`
    /// (by its exit code, or with `--json` its document, see [`proves`]);
    /// else what it said, as [`verdict`] reads it.
    fn verify(
        &self,
        answer: Answer,
        mut command: Command,
        commit: &str,
        receipt: &Receipt,
        under: crate::derived::Under,
    ) -> Check {
        if answer == Answer::Document {
            command.arg("--json");
        }
        self.verified.set(self.verified.get() + 1);
        match process::run(&mut command, None, self.timeout()) {
            Err(error) => Check::Unverified {
                why: format!("sykli verify could not run: {error:#}"),
            },
            Ok(output) if output.status.success() => {
                let proven = match answer {
                    Answer::Exit => Ok(()),
                    Answer::Document => proves(&output.stdout, commit),
                };
                match proven {
                    Ok(()) => {
                        self.proved(commit, receipt, under);
                        Check::Passed
                    }
                    Err(why) => Check::Unverified { why },
                }
            }
            Ok(output) => verdict(output.status.code(), output.failure_detail()),
        }
    }

    /// What the entry's gate says of `commit` (whose tree is `tree`):
    /// `sykli verify <receipt> --contract <gate> --commit <commit>`, handed
    /// the clone's facts when sykli takes them. Only receipts that say they
    /// passed are asked about, best first, until one verifies: a receipt
    /// that says the run failed is not a pass, and needs no sykli to say so.
    /// With no receipt of the tree it was never run here (`alive`: and the
    /// checkout it was made in still exists).
    pub fn check_gated(
        &self,
        gate: &Gate,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        alive: bool,
    ) -> Check {
        let key = (gate.entry.clone(), commit.to_owned());
        if let Some(known) = self.judged.borrow().get(&key) {
            return known.clone();
        }
        let check = self.ask_gate(gate, commit, tree, receipts, alive);
        // Only what sykli said is kept: what it could not say may be said
        // when it is asked again, and with no receipt it was never asked.
        if !matches!(
            check,
            Check::Unverified { .. } | Check::Unchecked | Check::UncheckedGone
        ) {
            self.judged.borrow_mut().insert(key, check.clone());
        }
        check
    }

    fn ask_gate(
        &self,
        gate: &Gate,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        alive: bool,
    ) -> Check {
        let candidates = receipts.of(tree);
        if candidates.is_empty() {
            return if alive {
                Check::Unchecked
            } else {
                Check::UncheckedGone
            };
        }
        // The commit's gate: derived from its own sykli.json, else from what
        // sykli init writes for its tree. Read only now, when a receipt of
        // its tree is here: a pass or a failure counts only under it.
        let at = match self.gate_at(gate, commit) {
            Ok(at) => at,
            Err(why) => {
                return Check::Unverified {
                    why: format!("its gate cannot be read: {why}"),
                }
            }
        };
        let derived = match &at {
            crate::derived::GateAt::Derived(derived) => derived,
            crate::derived::GateAt::Missing(why) => {
                return Check::Unverified {
                    why: format!("no gate judges it: {why}"),
                }
            }
        };
        let passing: Vec<&Receipt> = candidates
            .iter()
            .filter(|receipt| matches!(receipt.evidence.outcome.as_str(), "passed" | "cached"))
            .collect();
        let mut verdicts = Vec::new();
        if !passing.is_empty() {
            let answer = match self.answer() {
                Ok(answer) => answer,
                Err(why) => return Check::Unverified { why },
            };
            let facts: Vec<&std::ffi::OsStr> = if self.takes_facts.get().copied().unwrap_or(false) {
                vec![
                    "--repo-root".as_ref(),
                    gate.top.as_os_str(),
                    "--git-common-dir".as_ref(),
                    gate.common.as_os_str(),
                ]
            } else {
                Vec::new()
            };
            let mut written = None;
            for receipt in passing.iter().take(MAX_ASKED) {
                if !self.may_verify() {
                    if verdicts.is_empty() {
                        verdicts.push((
                            receipt.at,
                            Check::Unverified {
                                why: "sykli was not asked again".to_owned(),
                            },
                        ));
                    }
                    break;
                }
                // A run under the repository's own contract at the commit
                // (CI's `sykli run sykli.json`) is verified against it, read
                // from the commit; every other against the gate.
                let own = derived.own.as_ref().filter(|own| {
                    own.ran(
                        receipt.contract_hash.as_deref(),
                        receipt.lock_digest.as_deref(),
                    )
                });
                let (contract, under) = match own {
                    Some(_) => (PathBuf::from("sykli.json"), crate::derived::Under::Own),
                    None => {
                        match written.get_or_insert_with(|| at.contract(&self.derived, false)) {
                            Ok(path) => (path.clone(), crate::derived::Under::Derived),
                            Err(why) => {
                                verdicts.push((
                                    receipt.at,
                                    Check::Unverified {
                                        why: format!("its gate cannot be written: {why}"),
                                    },
                                ));
                                break;
                            }
                        }
                    }
                };
                let mut command = self.command();
                command
                    .arg("verify")
                    .arg(&receipt.path)
                    .arg("--contract")
                    .arg(&contract)
                    .args(&facts)
                    .args(["--commit", commit])
                    .current_dir(&gate.top);
                match self.verify(answer, command, commit, receipt, under) {
                    Check::Passed => return Check::Passed,
                    check => verdicts.push((receipt.at, check)),
                }
            }
        }
        // The runs that failed are their receipts' word, unasked, and a
        // failure of the commit only under its gate or its own contract, as
        // a pass is: a run under any other contract (a copy from before, a
        // contract elsewhere) that failed says nothing of the gate.
        for receipt in candidates
            .iter()
            .filter(|receipt| !matches!(receipt.evidence.outcome.as_str(), "passed" | "cached"))
        {
            let (hash, lock) = (
                receipt.contract_hash.as_deref(),
                receipt.lock_digest.as_deref(),
            );
            let said = format!(
                "its receipt says the run {} ({})",
                receipt.evidence.outcome,
                receipt.path.display()
            );
            let ours = derived.ran(hash, lock)
                || derived.own.as_ref().is_some_and(|own| own.ran(hash, lock));
            verdicts.push((
                receipt.at,
                if ours {
                    Check::Failed { why: said }
                } else {
                    Check::OtherContract {
                        why: match hash {
                            Some(hash) => format!("{said}, under the contract {hash}"),
                            None => format!("{said}, under no contract it names"),
                        },
                    }
                },
            ));
        }
        let mut check = settle(verdicts);
        if passing.len() > MAX_ASKED {
            if let Check::Failed { why }
            | Check::Unverified { why }
            | Check::OtherContract { why } = &mut check
            {
                why.push_str(&format!(
                    " (sykli was asked about the best {MAX_ASKED} of {} receipts of its tree)",
                    passing.len()
                ));
            }
        }
        check
    }

    /// What sykli says about `commit` (whose tree is `tree`) in `repo`:
    /// `sykli verify <receipt> --contract <contract> --commit <commit>`, the
    /// contract the commit has. It runs in a checkout at the commit with
    /// nothing beside it when there is one (`checkouts`), else in the
    /// checkout the receipt is in: there it only resolves the commit.
    ///
    /// sykli proves it (see [`Sykli::verify`]), or refuses it in its own
    /// words, judging the commit alone ([`verdict`]). A receipt's own
    /// `outcome` is never read as a pass, a sykli that cannot bind a commit
    /// never passes anything, and a commit with no contract cannot be checked.
    /// With no receipt, `alive` says whether the checkout it was made in, and
    /// so its receipts, still exists: when not, it cannot be observed.
    ///
    /// A repository whose origin names an entry (made [`Sykli::with_home`])
    /// is judged by the entry's gate instead: [`Sykli::check_gated`].
    pub fn check(
        &self,
        repo: &Path,
        commit: &str,
        tree: &str,
        receipts: &Receipts,
        checkouts: &[Worktree],
        alive: bool,
    ) -> Check {
        if let Some(gate) = self.gate_for(repo) {
            return self.check_gated(&gate, commit, tree, receipts, alive);
        }
        let candidates = receipts.of(tree);
        let contract = ["sykli.json", "sykli.rs"]
            .into_iter()
            .find(|name| git(repo, &["cat-file", "-e", &format!("{commit}:{name}")]).is_some());
        if candidates.is_empty() {
            return if contract.is_some() && alive {
                Check::Unchecked
            } else if contract.is_some() {
                Check::UncheckedGone
            } else {
                Check::Unverified {
                    why: "no sykli contract (sykli.json or sykli.rs) in this commit".to_owned(),
                }
            };
        }
        let answer = match self.answer() {
            Ok(answer) => answer,
            Err(why) => return Check::Unverified { why },
        };
        let Some(contract) = contract else {
            return Check::Unverified {
                why: "no sykli contract (sykli.json or sykli.rs) in this commit".to_owned(),
            };
        };
        let at_commit = checkouts
            .iter()
            .filter(|w| w.head.as_deref() == Some(commit))
            .map(|w| w.path.as_path())
            .find(|path| clean(path));
        // Every receipt of the tree, best first, until one passes: a newer run
        // of it under another contract, or a failing one, never hides a pass.
        let mut verdicts = Vec::new();
        for receipt in candidates.iter().take(MAX_ASKED) {
            let mut command = self.command();
            command
                .arg("verify")
                .arg(&receipt.path)
                .args(["--contract", contract, "--commit", commit])
                .current_dir(at_commit.unwrap_or(&receipt.checkout));
            match self.verify(answer, command, commit, receipt, crate::derived::Under::Own) {
                Check::Passed => return Check::Passed,
                check => verdicts.push((receipt.at, check)),
            }
        }
        let mut check = settle(verdicts);
        // Said when the cap left receipts unasked: one of them might pass.
        if candidates.len() > MAX_ASKED {
            if let Check::Failed { why }
            | Check::Unverified { why }
            | Check::Stale { why }
            | Check::OtherContract { why } = &mut check
            {
                why.push_str(&format!(
                    " (sykli was asked about the best {MAX_ASKED} of {} receipts of its tree)",
                    candidates.len()
                ));
            }
        }
        check
    }
}

/// What the receipts of one tree say when none of them proved it, from
/// (when the run started, what was said of it): what sykli could not say of
/// one, since that one might have passed; else the newest run's word, a run
/// that failed or one that passed under another contract.
fn settle(mut verdicts: Vec<(i64, Check)>) -> Check {
    let unsaid = |check: &Check| matches!(check, Check::Unverified { .. } | Check::Stale { .. });
    if let Some(at) = verdicts.iter().position(|(_, check)| unsaid(check)) {
        return verdicts.swap_remove(at).1;
    }
    verdicts
        .into_iter()
        .max_by_key(|(at, _)| *at)
        .map_or(Check::Unchecked, |(_, check)| check)
}

/// What a non-zero `sykli verify --commit` means. sykli judges the commit
/// alone, so it refuses only the commit itself (exit 1 or 3, the first
/// failing check on stderr), and exit 4 says the receipt passed under
/// another contract or lock than the one it is judged by now: not proven,
/// never failed. Exit 2 is that it could not verify (a bad revision,
/// arguments, not a receipt), and any other end (a crash, a kill) says
/// nothing about the commit either.
fn verdict(exit: Option<i32>, why: String) -> Check {
    match exit {
        Some(1 | 3) => Check::Failed { why },
        Some(4) => Check::OtherContract { why },
        Some(2) => Check::Unverified {
            why: format!("sykli verify could not verify: {why}"),
        },
        // A crash, a kill or an exit sykli does not document says nothing
        // about the commit.
        _ => Check::Unverified {
            why: format!("sykli verify could not say: {why}"),
        },
    }
}

/// Whether `checkout` has nothing beside its HEAD, sykli's own `.sykli/` aside.
fn clean(checkout: &Path) -> bool {
    git(
        checkout,
        &["status", "--porcelain", "--", ".", ":(exclude).sykli"],
    )
    .is_some_and(|status| status.is_empty())
}

/// What sykli says of a run, from `sykli status --run <id> --json`
/// (`sykli-run-status.v1`, KATANA D3): `sykli says run <id> completed: 1
/// passed, 1 cached`. Its words, labelled as its; the task states are
/// counted in a fixed order, cached apart from passed. A run that ended in
/// any state but `completed` (interrupted, lost, indeterminate, …) proved
/// nothing and failed nothing, and is said so; only a run that completed
/// with a failing task is a failure, and its tasks say that.
pub fn describe_run_status(run: &str, status: &Value) -> String {
    let word = status["state"].as_str().unwrap_or("in an unknown state");
    let state = match word {
        "requested" | "running" => format!("is {word}"),
        other => other.to_owned(),
    };
    let mut counts: Vec<(String, usize)> = [
        "passed",
        "cached",
        "failed",
        "interrupted",
        "lost",
        "running",
        "pending",
    ]
    .iter()
    .map(|name| ((*name).to_owned(), 0))
    .collect();
    for task in status["tasks"].as_array().map_or(&[][..], Vec::as_slice) {
        let name = task["state"].as_str().unwrap_or("unknown");
        match counts.iter_mut().find(|(known, _)| known == name) {
            Some((_, n)) => *n += 1,
            None => counts.push((name.to_owned(), 1)),
        }
    }
    let tasks: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .map(|(name, n)| format!("{n} {name}"))
        .collect();
    let tasks = if tasks.is_empty() {
        "no tasks".to_owned()
    } else {
        tasks.join(", ")
    };
    let unknown = if run_ended_unknown(status) {
        " (not proven, not failed: unknown)"
    } else {
        ""
    };
    format!("sykli says run {run} {state}: {tasks}{unknown}")
}

/// Whether sykli says the run is over without having completed: what it ran
/// is unknown, never a failure.
pub fn run_ended_unknown(status: &Value) -> bool {
    !matches!(
        status["state"].as_str(),
        Some("requested" | "running" | "completed")
    )
}

/// The receipt a `sykli-run-status.v1` names, once the run has one: a
/// relative path is taken from `from`, the checkout sykli was asked in.
pub fn receipt_of_status(status: &Value, from: &Path) -> Option<ReceiptRef> {
    let path = Path::new(status["receipt"]["path"].as_str()?);
    Some(ReceiptRef {
        path: from.join(path).to_string_lossy().into_owned(),
        digest: status["receipt"]["digest"].as_str().map(str::to_owned),
    })
}

/// What sykli says ran (`subject.tree`, `subject.dirty`) in a
/// `sykli-run-status.v1`, once it says.
pub fn ran_of_status(status: &Value) -> Option<crate::sessions::Ran> {
    Some(crate::sessions::Ran {
        tree: status["subject"]["tree"].as_str()?.to_owned(),
        dirty: status["subject"]["dirty"].as_bool()?,
    })
}

/// What `sykli status --help` says: whether it has durable runs to ask
/// about (`--run`, KATANA D3), whether it judges what applies to a commit
/// (`--commit` and `--contract`, `sykli-commit-status.v1`), and whether it
/// takes the repository's facts (`--repo-root` and `--git-common-dir`).
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusHelp {
    pub runs: bool,
    pub commit: bool,
    pub facts: bool,
}

/// Ask `sykli status --help` once.
pub fn sykli_status_help() -> StatusHelp {
    let help = process::run(
        Command::new("sykli").args(["status", "--help"]),
        None,
        process::LOCAL,
    )
    .ok()
    .filter(|output| output.status.success())
    .map(|output| {
        format!(
            "{}{}",
            output.stdout_text(),
            String::from_utf8_lossy(&output.stderr)
        )
    })
    .unwrap_or_default();
    StatusHelp {
        runs: process::names_flag(&help, "--run"),
        commit: process::names_flag(&help, "--commit") && process::names_flag(&help, "--contract"),
        facts: process::names_flag(&help, "--repo-root")
            && process::names_flag(&help, "--git-common-dir"),
    }
}

/// `sykli status --run <id> --json`, from the checkout `repo` (handed
/// `--repo-root` and `--git-common-dir` when sykli takes them and `common`
/// is known): the run's status, or why sykli could not say.
pub fn sykli_run_status(
    repo: &Path,
    common: Option<&Path>,
    help: StatusHelp,
    run: &str,
) -> Result<Value, String> {
    let mut command = Command::new("sykli");
    command.args(["status", "--run", run]);
    if let (true, Some(common)) = (help.facts, common) {
        command
            .arg("--repo-root")
            .arg(repo)
            .arg("--git-common-dir")
            .arg(common);
    }
    let output = process::run(
        command.arg("--json").current_dir(repo),
        None,
        process::LOCAL,
    )
    .map_err(|error| format!("{error:#}"))?;
    if !output.status.success() {
        return Err(output.failure_detail());
    }
    let status: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("its answer is not JSON ({error})"))?;
    if status["schema"] != "sykli-run-status.v1" {
        return Err(format!(
            "its answer is not sykli-run-status.v1 (schema {})",
            status["schema"]
        ));
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The receipts of a clone, from its files alone: those sykli keeps
    /// under the common dir, and in each worktree (git names the linked ones
    /// in `<common>/worktrees/*/gitdir`, absolute or relative) those `sykli
    /// run` writes and the one `ohjaa gate` saves.
    #[test]
    fn receipts_are_found_from_the_layout_alone() {
        let root = std::env::temp_dir().join(format!("ohjaa-layout-r-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |path: PathBuf, tree: &str, outcome: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!(
                    r#"{{"schema":"sykli-receipt.v1","outcome":"{outcome}","started_at_ms":1,"subject":{{"tree_oid":"{tree}","dirty":false}}}}"#
                ),
            )
            .unwrap();
        };
        let (main, near, far) = (root.join("main"), root.join("near"), root.join("far"));
        let common = main.join(".git");
        write(common.join("sykli/receipts/rcpt_a.json"), "kept", "passed");
        write(main.join(".sykli/receipts/rcpt_b.json"), "ran", "failed");
        write(near.join(".sykli/receipt.json"), "gate", "passed");
        write(far.join(".sykli/receipts/rcpt_c.json"), "far", "passed");
        for (id, gitdir) in [
            ("near", "../../../../near/.git".to_owned()),
            ("far", format!("{}\n", far.join(".git").display())),
        ] {
            let dir = common.join("worktrees").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("gitdir"), gitdir).unwrap();
        }
        let found = receipts_of_layout(&common);
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
        assert_eq!(
            found["kept"].checkout,
            canonical(&main),
            "run from the main checkout"
        );
        assert_eq!(found["ran"].evidence.outcome, "failed");
        assert_eq!(found["gate"].checkout, canonical(&near));
        assert_eq!(
            found["gate"].path,
            canonical(&near).join(".sykli/receipt.json")
        );
        assert_eq!(found["far"].checkout, canonical(&far));
        // A bare clone has no main checkout; what sykli keeps is still read.
        let bare = root.join("bare.git");
        write(bare.join("sykli/receipts/rcpt_d.json"), "bare", "passed");
        assert_eq!(receipts_of_layout(&bare)["bare"].checkout, bare);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// G1c: the receipt a run concluded with is kept in the run's own folder
    /// under the common dir, `<common>/sykli/runs/<run>/rcpt_<sha256>.json`,
    /// so it is found once the worktree that ran it is removed; and a
    /// receipt kept in two places (the worktree's `.sykli/receipts/` and its
    /// run's folder, both named by its digest) is one receipt, found once.
    #[test]
    fn a_receipt_sykli_kept_with_its_run_outlives_the_worktree_and_counts_once() {
        let root = std::env::temp_dir().join(format!("ohjaa-layout-runs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        let common = main.join(".git");
        let gone = root.join("gone");
        let admin = common.join("worktrees/gone");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(
            admin.join("gitdir"),
            format!("{}\n", gone.join(".git").display()),
        )
        .unwrap();
        let body = |tree: &str| {
            format!(
                r#"{{"schema":"sykli-receipt.v1","outcome":"passed","started_at_ms":1,"subject":{{"tree_oid":"{tree}","dirty":false}}}}"#
            )
        };
        let named = |n: char| format!("rcpt_{}.json", n.to_string().repeat(64));
        let put = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        // A run in a worktree that is gone since: only its run's copy is left.
        put(
            common.join("sykli/runs/run1").join(named('a')),
            &body("landed"),
        );
        // One still here: the same receipt in the worktree and in its run.
        put(main.join(".sykli/receipts").join(named('b')), &body("here"));
        put(
            common.join("sykli/runs/run2").join(named('b')),
            &body("here"),
        );
        // sykli's own leftovers and journal files are no receipts.
        put(
            common.join("sykli/runs/.run3-aside").join(named('c')),
            &body("aside"),
        );
        put(common.join("sykli/runs/run2/coordinator.log"), "");
        let found = receipts_of_layout(&common);
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
        assert_eq!(
            found["landed"].path,
            common.join("sykli/runs/run1").join(named('a'))
        );
        assert_eq!(found["landed"].checkout, canonical(&main));
        assert_eq!(found.of("here").len(), 1, "one receipt, found once");
        assert_eq!(
            found["here"].path,
            canonical(&main).join(".sykli/receipts").join(named('b')),
            "where it is first found: the worktree that ran it"
        );
        assert!(found.of("aside").is_empty());
        let worktree = Worktree {
            path: canonical(&main),
            head: None,
            branch: None,
        };
        let at_leaving = receipts_from(
            [(
                worktree.path.join(".sykli/receipts"),
                worktree.path.as_path(),
            )]
            .into_iter()
            .chain(
                kept_by_sykli(&common)
                    .into_iter()
                    .map(|dir| (dir, main.as_path())),
            ),
        );
        assert_eq!(at_leaving.of("landed").len(), 1);
        assert_eq!(at_leaving.of("here").len(), 1);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The review of #60, finding 2: a file under a kept receipt's name that
    /// is not that receipt (cut short, or another receipt) never hides it:
    /// only the same bytes are the same receipt. One that cannot be read is
    /// said, never taken for the receipt.
    #[test]
    fn a_copy_under_a_receipts_name_that_is_not_it_never_hides_it() {
        let root = std::env::temp_dir().join(format!("ohjaa-layout-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let main = root.join("main");
        let common = main.join(".git");
        let body = |tree: &str, outcome: &str| {
            format!(
                r#"{{"schema":"sykli-receipt.v1","outcome":"{outcome}","started_at_ms":1,"subject":{{"tree_oid":"{tree}","dirty":false}}}}"#
            )
        };
        let named = |n: char| format!("rcpt_{}.json", n.to_string().repeat(64));
        let put = |path: PathBuf, text: &str| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        // Cut short in the worktree, whole with its run.
        put(
            main.join(".sykli/receipts").join(named('a')),
            "{\"truncated",
        );
        put(
            common.join("sykli/runs/run1").join(named('a')),
            &body("cut", "passed"),
        );
        // Another receipt under the same name in the worktree.
        put(
            main.join(".sykli/receipts").join(named('b')),
            &body("other", "failed"),
        );
        put(
            common.join("sykli/runs/run2").join(named('b')),
            &body("other", "passed"),
        );
        // The same bytes twice: one receipt.
        put(
            main.join(".sykli/receipts").join(named('c')),
            &body("same", "passed"),
        );
        put(
            common.join("sykli/runs/run3").join(named('c')),
            &body("same", "passed"),
        );
        let found = receipts_of_layout(&common);
        assert_eq!(found.of("cut").len(), 1);
        assert_eq!(
            found["cut"].path,
            common.join("sykli/runs/run1").join(named('a'))
        );
        let outcomes: Vec<&str> = found
            .of("other")
            .iter()
            .map(|receipt| receipt.evidence.outcome.as_str())
            .collect();
        assert_eq!(outcomes, ["passed", "failed"], "both kept, best first");
        assert_eq!(found.of("same").len(), 1);
        let canonical = |path: &Path| std::fs::canonicalize(path).unwrap();
        let unreadable = canonical(&main).join(".sykli/receipts").join(named('a'));
        assert_eq!(found.unreadable().len(), 1, "{:?}", found.unreadable());
        assert!(
            found.unreadable()[0].starts_with(&unreadable.display().to_string()),
            "{:?}",
            found.unreadable()
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn repo(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ohjaa-life-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "T"],
            vec!["commit", "-q", "--allow-empty", "-m", "start"],
        ] {
            assert!(Command::new("git")
                .args(&args)
                .current_dir(&dir)
                .status()
                .unwrap()
                .success());
        }
        dir
    }

    fn commit(dir: &Path, file: &str, message: &str) -> String {
        std::fs::write(dir.join(file), message).unwrap();
        assert!(Command::new("git")
            .args(["add", file])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-q", "-m", message])
            .current_dir(dir)
            .status()
            .unwrap()
            .success());
        git(dir, &["rev-parse", "HEAD^{tree}"]).unwrap()
    }

    /// A receipt for a run of `ran` (the tree that ran) with HEAD at `head`.
    fn receipt_ran(
        dir: &Path,
        name: &str,
        ran: &str,
        head: &str,
        outcome: &str,
        at: i64,
        dirty: bool,
    ) {
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join(format!("{name}.json")),
            format!(
                r#"{{"schema":"sykli-receipt.v1","contract_hash":"x","started_at_ms":{at},
                   "subject":{{"repository":"/r","tree_oid":"{ran}","inputs_digest":"d",
                   "head_tree_oid":"{head}","dirty":{dirty}}},"tasks":[],"outcome":"{outcome}"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_receipt_is_for_the_tree_that_ran_and_a_clean_pass_outranks_a_newer_dirty_run() {
        let dir = repo("receipt-choice");
        let tree = commit(&dir, "a.txt", "checked");
        receipt_ran(&dir, "clean", &tree, &tree, "passed", 1, false);
        // An experiment run on top of HEAD, then reverted: it ran another tree.
        receipt_ran(&dir, "experiment", "e0e0", &tree, "failed", 2, true);
        // A newer run of the same tree, with something uncommitted beside it.
        receipt_ran(&dir, "dirty", &tree, &tree, "passed", 3, true);
        let receipts = receipts_in([dir.as_path()]);
        let chosen = &receipts[&tree];
        assert!(chosen.path.ends_with("clean.json"), "{:?}", chosen.path);
        assert!(!chosen.evidence.dirty);
        assert_eq!(
            receipts["e0e0"].evidence.outcome, "failed",
            "filed under what ran"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pass_made_before_the_commit_outranks_a_newer_failing_run_of_its_tree() {
        let dir = repo("receipt-precommit");
        let parent = commit(&dir, "a.txt", "parent");
        let tree = commit(&dir, "a.txt", "the phase");
        // sykli's pre-commit hook runs the tree being committed while HEAD is
        // still the parent: sykli calls that dirty, yet it ran exactly this tree.
        receipt_ran(&dir, "pre-commit", &tree, &parent, "passed", 1, true);
        // A newer run of the same tree failed (a flaky task).
        receipt_ran(&dir, "flake", &tree, &tree, "failed", 2, false);
        // A newer pass with something beside the tree (a modified submodule).
        receipt_ran(&dir, "beside", &tree, &tree, "passed", 3, true);
        let receipts = receipts_in([dir.as_path()]);
        let chosen = &receipts[&tree];
        assert!(
            chosen.path.ends_with("pre-commit.json"),
            "{:?}",
            chosen.path
        );
        assert_eq!(chosen.evidence.outcome, "passed");
        assert!(
            !chosen.evidence.dirty,
            "a run of exactly the commit's tree, made before the commit, is not dirty"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_refusal_fails_the_commit_only_when_it_is_about_the_commit() {
        let why = |check: &str| format!("refused: {check} expected a but got b");
        // sykli judges the commit alone: a refusal is of the commit.
        for (exit, check) in [(1, "outcome"), (3, "commit"), (3, "submodules")] {
            assert!(
                matches!(verdict(Some(exit), why(check)), Check::Failed { .. }),
                "{check}"
            );
        }
        // A run that passed under another contract or lock: not proven
        // against this one, and never failed.
        for check in ["contract", "lock"] {
            assert_eq!(
                verdict(Some(4), why(check)),
                Check::OtherContract { why: why(check) },
                "{check}"
            );
        }
        assert!(matches!(
            verdict(Some(2), "fatal: bad revision".to_owned()),
            Check::Unverified { .. }
        ));
        // A sykli that crashed or was killed said nothing about the commit.
        for exit in [None, Some(101), Some(127)] {
            assert!(
                matches!(
                    verdict(exit, "thread 'main' panicked".to_owned()),
                    Check::Unverified { .. }
                ),
                "{exit:?}"
            );
        }
    }

    #[test]
    fn a_json_verdict_proves_the_receipts_identity_for_the_commit_and_its_claimed_outcome() {
        let commit = "c0ffee";
        let identity = r#""schema","contract","source","consistency""#;
        let today = r#""schema","contract","source","consistency","outcome""#;
        let outcome = |claimed: &str, ok: bool| {
            format!(
                r#"{{"name":"outcome","ok":{ok},"expected":"passed or cached with complete task records","actual":"{claimed} with 2 task records"}}"#
            )
        };
        // schema, subject kind, verified, established, not established, checks.
        let document = |schema: &str,
                        kind: &str,
                        verified: bool,
                        tiers: &str,
                        not: &str,
                        checks: &str| {
            format!(
                r#"{{"schema":"{schema}","subject":{{"kind":"{kind}","rev":"{commit}"}},"verified":{verified},"exit":0,"checks":[{checks}],"trust":{{"established":[{tiers}],"not_established":[{not}]}}}}"#
            )
        };
        let unsigned = r#""provenance","producer","execution""#;
        let claim = r#""outcome","provenance","producer","execution""#;
        let passed = outcome("passed", true);
        // sykli main today lists the outcome as established; sykli as it is
        // moving to does not: it is the receipt's claim. Both prove it, as
        // claimed, passed or cached.
        for (tiers, not, checks) in [
            (today, unsigned, passed.clone()),
            (identity, claim, passed.clone()),
            (identity, claim, outcome("cached", true)),
        ] {
            let verdict = document("sykli-verify.v1", "commit", true, tiers, not, &checks);
            assert_eq!(proves(verdict.as_bytes(), commit), Ok(()), "{verdict}");
        }
        let refused = [
            (
                document("sykli-verify.v2", "commit", true, today, unsigned, &passed),
                "sykli-verify.v2",
            ),
            (
                document(
                    "sykli-verify.v1",
                    "checkout",
                    true,
                    today,
                    unsigned,
                    &passed,
                ),
                "not the commit",
            ),
            (
                document("sykli-verify.v1", "commit", false, today, unsigned, &passed),
                "verified",
            ),
            // An identity tier missing, whatever is said of the outcome.
            (
                document(
                    "sykli-verify.v1",
                    "commit",
                    true,
                    r#""schema","contract","consistency","outcome""#,
                    unsigned,
                    &passed,
                ),
                "source",
            ),
            (
                document(
                    "sykli-verify.v1",
                    "commit",
                    true,
                    r#""schema","source","consistency""#,
                    claim,
                    &passed,
                ),
                "contract",
            ),
            // No claimed outcome, or one that is not a pass.
            (
                document("sykli-verify.v1", "commit", true, identity, claim, ""),
                "outcome",
            ),
            (
                document(
                    "sykli-verify.v1",
                    "commit",
                    true,
                    identity,
                    claim,
                    &outcome("failed", false),
                ),
                "failed",
            ),
            ("not json".to_owned(), "no verdict"),
        ];
        for (verdict, said) in refused {
            let why = proves(verdict.as_bytes(), commit).unwrap_err();
            assert!(why.contains(said), "{said}: {why}");
        }
        // A sykli with `claimed_outcome` is read by that field alone; the
        // outcome check's words are only the fallback for one without it.
        let with_field = |claimed: &str, checks: &str| {
            document("sykli-verify.v1", "commit", true, identity, claim, checks).replacen(
                "\"checks\"",
                &format!("\"claimed_outcome\":\"{claimed}\",\"checks\""),
                1,
            )
        };
        let worded_otherwise = outcome("as claimed,", true);
        for verdict in [
            with_field("passed", &worded_otherwise),
            with_field("cached", ""),
        ] {
            assert_eq!(proves(verdict.as_bytes(), commit), Ok(()), "{verdict}");
        }
        let why = proves(with_field("failed", &passed).as_bytes(), commit).unwrap_err();
        assert!(why.contains("claims failed"), "{why}");
        // Another commit's verdict proves nothing of this one.
        let other = document("sykli-verify.v1", "commit", true, today, unsigned, &passed);
        assert!(proves(other.as_bytes(), "beef").is_err());
    }

    #[test]
    fn worktrees_are_read_from_gits_porcelain() {
        let dir = std::env::temp_dir();
        let porcelain = format!(
            "worktree /nonexistent/bare\nbare\n\nworktree {d}\nHEAD abc\nbranch refs/heads/main\n\n\
             worktree /nonexistent/gone\nHEAD def\ndetached\nprunable gitdir file points to non-existent location",
            d = dir.display()
        );
        let found = parse_worktrees(&porcelain);
        assert_eq!(
            found,
            [Worktree {
                path: std::fs::canonicalize(&dir).unwrap(),
                head: Some("abc".to_owned()),
                branch: Some("refs/heads/main".to_owned()),
            }],
            "bare entries and checkouts whose folder is gone are left out"
        );
    }

    #[test]
    fn created_here_reads_gits_own_reflog_wording() {
        // Written by git 2.54 for each kind of move.
        for made in [
            "commit (initial): start",
            "commit: mine",
            "commit (amend): mine, amended",
            "commit (merge): Merge branch 'side'",
            "cherry-pick: side-a",
            "revert: Revert \"side-a\"",
            "pull -q --rebase (pick): mine",
            "rebase (reword): mine",
            "rebase -i (squash): # This is a combination of 2 commits.",
            "pull -q --no-rebase --no-edit: Merge made by the 'ort' strategy.",
            "merge side: Merge made by the 'ort' strategy.",
        ] {
            assert!(created_here(made), "{made}");
        }
        for moved in [
            "clone: from /somewhere",
            "pull -q: Fast-forward",
            "merge side: Fast-forward",
            "checkout: moving from side to main",
            "reset: moving to HEAD~1",
            "pull -q --rebase (start): checkout 2ecb28b",
            "pull -q --rebase (finish): returning to refs/heads/main",
            "rebase (abort): returning to refs/heads/main",
        ] {
            assert!(!created_here(moved), "{moved}");
        }
    }

    #[test]
    fn a_receipt_of_another_schema_is_not_evidence() {
        let dir = repo("schema");
        let tree = commit(&dir, "a.txt", "x");
        let receipts = dir.join(".sykli/receipts");
        std::fs::create_dir_all(&receipts).unwrap();
        std::fs::write(
            receipts.join("r.json"),
            format!(r#"{{"schema":"other.v1","subject":{{"head_tree_oid":"{tree}"}},"outcome":"passed"}}"#),
        )
        .unwrap();
        assert!(receipts_in([dir.as_path()]).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// sykli's status of a run, said as sykli's, the task states counted in
    /// a fixed order: cached apart from passed, never folded into it.
    #[test]
    fn a_run_status_is_said_as_sykli_says_it() {
        let status = serde_json::json!({
            "schema": "sykli-run-status.v1",
            "run": "r1",
            "state": "running",
            "subject": {"tree": "t1", "commit": null, "dirty": true},
            "tasks": [
                {"name": "a", "state": "cached"},
                {"name": "b", "state": "passed"},
                {"name": "c", "state": "running"},
                {"name": "d", "state": "odd"},
                {"name": "e", "state": "cached"},
            ],
            "receipt": null,
        });
        assert_eq!(
            describe_run_status("r1", &status),
            "sykli says run r1 is running: 1 passed, 2 cached, 1 running, 1 odd"
        );
        assert_eq!(receipt_of_status(&status, Path::new("/r")), None);
        assert_eq!(
            ran_of_status(&status),
            Some(crate::sessions::Ran {
                tree: "t1".to_owned(),
                dirty: true
            })
        );
        // Over, but not completed: unknown, never failed.
        for state in ["interrupted", "lost", "indeterminate"] {
            let done = serde_json::json!({"state": state, "tasks": []});
            assert!(run_ended_unknown(&done), "{state}");
            assert_eq!(
                describe_run_status("r2", &done),
                format!("sykli says run r2 {state}: no tasks (not proven, not failed: unknown)")
            );
        }
        let completed = serde_json::json!({
            "state": "completed",
            "tasks": [{"name": "a", "state": "failed"}],
            "receipt": {"path": ".sykli/receipts/rcpt_ab.json", "digest": "ab"},
        });
        assert!(!run_ended_unknown(&completed));
        assert_eq!(
            describe_run_status("r3", &completed),
            "sykli says run r3 completed: 1 failed"
        );
        // A relative receipt path is taken from where sykli was asked.
        assert_eq!(
            receipt_of_status(&completed, Path::new("/r")),
            Some(ReceiptRef {
                path: "/r/.sykli/receipts/rcpt_ab.json".to_owned(),
                digest: Some("ab".to_owned()),
            })
        );
    }
}
