//! The gate a commit is judged by, derived from the commit itself.
//!
//! An entry whose repository has its own `sykli.json` keeps no copy of it:
//! the gate for commit C is the repository's `sykli.json` at C (and the
//! `sykli.lock` beside it), with the entry's `[cargo_env]` lines given to
//! each cargo task that does not say where it builds (KATANA D10, the lines
//! `ohjaa adopt` used to write into a copy). It is written outside the
//! repository, under the entry, keyed by its digest
//! (`<entry>/derived/<digest>/sykli.json` and `sykli.lock`), and is a
//! function of C and the entry alone: the same C always gives the same gate,
//! and a contract changed at C gives another. A commit with no contract of
//! its own (no `sykli.json`, and no `sykli.rs`) is judged by what `sykli
//! init` writes for its tree ([`Inferred`]), given the same lines: asked of
//! sykli in a scratch checkout of the tree outside every repository
//! ([`Gates::inferred`]), and kept by tree in ohjaa's state once a run is
//! judged by it (`$XDG_STATE_HOME/ohjaa/inferred/<tree>/`), so it is asked
//! once. sykli's `init` declares every file the tree tracks, so a file added
//! is declared by the next commit's gate, with no adopt in between. An
//! entry's stored copy (`sykli.json` beside `repo.toml`, which adopt once
//! wrote) is never read: it drifted whenever a file was added.
//!
//! The gate is locked when the repository's own lock pins its contract at
//! C; a lock that does not is kept as it is (sykli then refuses to run the
//! gate, as it refuses the repository's own), and a repository with no lock
//! gets none. Never loosened: what the repository did not lock, ohjaa does
//! not lock.
//!
//! Commits and their files are read from git's object store by one `git
//! cat-file --batch` per clone ([`Objects`]), asked one object at a time.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::process;

/// SHA-256 of `bytes`, in lower-case hex (FIPS 180-4). ohjaa needs it to
/// name what sykli names: a contract's hash, a lock's digest. No hashing
/// crate is a dependency here; the tests check it against one.
pub fn sha256(bytes: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = bytes.to_vec();
    let bits = (bytes.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (at, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[at] = u32::from_be_bytes(*word);
        }
        for at in 16..64 {
            let s0 = w[at - 15].rotate_right(7) ^ w[at - 15].rotate_right(18) ^ (w[at - 15] >> 3);
            let s1 = w[at - 2].rotate_right(17) ^ w[at - 2].rotate_right(19) ^ (w[at - 2] >> 10);
            w[at] = w[at - 16]
                .wrapping_add(s0)
                .wrapping_add(w[at - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for at in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(K[at])
                .wrapping_add(w[at]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (word, add) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(add);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

/// The hash sykli gives a contract and pins in `sykli.lock` (and a receipt
/// names as `contract_hash`): the SHA-256 of its JSON written back with its
/// keys sorted and no blanks. `None` when it is not JSON.
pub fn contract_hash(bytes: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    Some(sha256(&serde_json::to_vec(&value).ok()?))
}

/// The hash a lock (`sykli-lock.v1`'s one hash, or `sykli-lock.v2`'s entry
/// for `name`) pins, as sykli reads it; `None` when it pins none.
pub fn pinned_in(lock: &[u8], name: &str) -> Option<String> {
    let lock: serde_json::Value = serde_json::from_slice(lock).ok()?;
    match lock["schema"].as_str()? {
        "sykli-lock.v1" => lock["contract_hash"].as_str().map(str::to_owned),
        "sykli-lock.v2" => lock["contracts"][name].as_str().map(str::to_owned),
        _ => None,
    }
}

/// A `sykli.lock` pinning `hash` for `sykli.json`, as `sykli lock` writes one.
fn fresh_lock(hash: &str) -> Vec<u8> {
    format!(
        "{{\n  \"schema\": \"sykli-lock.v2\",\n  \"contracts\": {{\n    \"sykli.json\": \
         \"{hash}\"\n  }}\n}}\n"
    )
    .into_bytes()
}

/// The env an entry gives each cargo task of a derived gate that leaves it
/// unsaid: its `[cargo_env]` in `repo.toml`, a variable and its value for
/// each of the places cargo builds in (KATANA D10).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overlay(Vec<(String, String)>);

impl Overlay {
    /// KATANA D10's lines (`${worktree_cache}/cargo-target` and
    /// `/cargo-build`): what every entry gives unless its `repo.toml` says
    /// otherwise.
    pub fn standard() -> Overlay {
        Overlay(crate::adopt::standard_lines())
    }

    pub fn lines(&self) -> &[(String, String)] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// From `repo.toml`'s `[cargo_env]` table: absent, [`Overlay::standard`]
    /// (an entry written before the table was, whose copy carried those
    /// lines); empty, no line; else exactly its lines. Only the variables
    /// of the two places cargo builds in are read.
    pub fn from_table(
        table: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<Overlay, String> {
        let Some(table) = table else {
            return Ok(Overlay::standard());
        };
        let vars = crate::adopt::overlay_vars();
        if let Some(key) = table.keys().find(|key| !vars.contains(&key.as_str())) {
            return Err(format!(
                "unknown key {key} in [cargo_env]; the keys read there are {}",
                vars.join(", ")
            ));
        }
        Ok(Overlay(
            vars.iter()
                .filter_map(|var| Some(((*var).to_owned(), table.get(*var)?.clone())))
                .collect(),
        ))
    }

    /// The `[cargo_env]` table for `repo.toml`; `none` says why it is empty.
    pub fn toml(&self, none: Option<&str>) -> String {
        let mut text = String::from(
            "\n# What each cargo task of the gate derived from the repository's own sykli.json\n\
             # is given, unless it says where it builds: a build directory per worktree\n\
             # (KATANA D10).\n",
        );
        if let Some(why) = none.filter(|_| self.is_empty()) {
            text.push_str(&format!("# None: {why}.\n"));
        }
        text.push_str("[cargo_env]\n");
        for (var, value) in &self.0 {
            text.push_str(&format!("{var} = {}\n", crate::flat_toml::quote(value)));
        }
        text
    }
}

/// What a commit holds of its own: its `sykli.json`, and the `sykli.lock`
/// beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Own {
    pub contract: Vec<u8>,
    pub lock: Option<Vec<u8>>,
}

/// The repository's own contract and lock at a commit, by the names a
/// receipt of a run under them gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnId {
    /// sykli's hash of the contract.
    pub hash: String,
    /// The SHA-256 of its lock file, when it has one.
    pub lock_digest: Option<String>,
}

impl OwnId {
    /// Whether a receipt naming this contract hash (and this lock digest,
    /// when it names one) ran under the repository's own contract. A
    /// receipt an older sykli wrote names no lock digest: `sykli verify`
    /// checks its lock, so only a digest it does name has to match here.
    pub fn ran(&self, contract_hash: Option<&str>, lock_digest: Option<&str>) -> bool {
        contract_hash == Some(self.hash.as_str())
            && lock_digest.is_none_or(|digest| Some(digest) == self.lock_digest.as_deref())
    }
}

/// Whether the repository's own lock pins its contract at the commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pinned {
    /// It has no `sykli.lock`: the gate has none either.
    NoLock,
    /// Its lock pins its contract: the gate is locked too.
    Yes,
    /// Its lock does not pin it (changed without `sykli lock`): kept as it
    /// is beside the gate, so sykli refuses to run it.
    No,
}

/// The gate derived for one commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Derived {
    pub contract: Vec<u8>,
    pub lock: Option<Vec<u8>>,
    /// The repository's own contract, when the gate is not it unchanged: a
    /// receipt of a run under it proves the commit as well (the same tasks
    /// and inputs, no line given), and is verified against it.
    pub own: Option<OwnId>,
    pub pinned: Pinned,
    /// The cargo tasks given lines; empty when the gate is the repository's
    /// own contract unchanged.
    pub given: Vec<String>,
    /// The repository's tasks that run cargo, and those that name
    /// `${worktree_cache}` themselves.
    pub cargo: Vec<String>,
    pub placeholder: Vec<String>,
    /// Why no line was given to a contract ohjaa cannot read.
    pub unread: Option<String>,
    /// SHA-256 of the contract and the lock: the folder it is kept in.
    pub digest: String,
    /// For a commit with no contract of its own: what `sykli init` wrote
    /// for its tree, the gate before the lines.
    pub inferred: Option<Inferred>,
}

/// What `sykli init` writes for a tree: the contract of a commit with no
/// `sykli.json` (and no `sykli.rs`) of its own, before the entry's lines.
/// sykli init declares every file the tree tracks (sykli#89), so it is a
/// function of the tree (and of the sykli that wrote it: once kept, it is
/// what the tree's gate is).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inferred {
    pub tree: String,
    pub init: Own,
}

/// The gate of a commit with no contract of its own: what `sykli init`
/// wrote for its tree, given the entry's lines as a repository's own
/// contract is, and locked as sykli init locked it. No receipt proves the
/// commit but one of a run under this gate: there is no contract of the
/// repository's own to verify one against.
pub fn derive_inferred(inferred: Inferred, overlay: &Overlay) -> Derived {
    let mut derived = derive(&inferred.init, overlay);
    derived.own = None;
    derived.inferred = Some(inferred);
    derived
}

/// What a commit holds of its own contract, by the entries of its top tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holds {
    /// Its `sykli.json`, and the `sykli.lock` beside it.
    Own(Own),
    /// A `sykli.rs` and no `sykli.json`: a contract sykli emits, which no
    /// gate is derived from.
    Emitter,
    /// Neither: its gate is what `sykli init` writes for its tree.
    Neither { tree: String },
}

/// The gate for a commit whose own files are `own`: deterministic, and
/// changed by any change to them or to the entry's lines.
pub fn derive(own: &Own, overlay: &Overlay) -> Derived {
    let own_hash = contract_hash(&own.contract);
    let pinned = match &own.lock {
        None => Pinned::NoLock,
        Some(lock) if own_hash.is_some() && pinned_in(lock, "sykli.json") == own_hash => {
            Pinned::Yes
        }
        Some(_) => Pinned::No,
    };
    let (read, unread) = match crate::adopt::overlaid(&own.contract, overlay.lines()) {
        Ok(read) => (read, None),
        Err(why) => (
            crate::adopt::Overlaid {
                bytes: None,
                given: Vec::new(),
                cargo: Vec::new(),
                placeholder: Vec::new(),
            },
            Some(why),
        ),
    };
    let (contract, lock, own_id) = match read.bytes {
        Some(bytes) => {
            let lock = match pinned {
                Pinned::Yes => Some(fresh_lock(&sha256(&bytes))),
                Pinned::NoLock => None,
                // Locked to the repository's own contract, which a gate
                // given lines is not: sykli refuses it, whatever the
                // repository's lock pinned.
                Pinned::No => own_hash.as_deref().map(fresh_lock),
            };
            let own_id = own_hash.map(|hash| OwnId {
                hash,
                lock_digest: own.lock.as_deref().map(sha256),
            });
            (bytes, lock, own_id)
        }
        // No line given: the gate is the repository's own contract and
        // lock, byte for byte.
        None => (own.contract.clone(), own.lock.clone(), None),
    };
    let mut keyed = contract.clone();
    keyed.push(0);
    match &lock {
        Some(lock) => {
            keyed.extend_from_slice(b"lock\0");
            keyed.extend_from_slice(lock);
        }
        None => keyed.extend_from_slice(b"no lock"),
    }
    Derived {
        digest: sha256(&keyed),
        contract,
        lock,
        own: own_id,
        pinned,
        given: read.given,
        cargo: read.cargo,
        placeholder: read.placeholder,
        unread,
        inferred: None,
    }
}

impl Derived {
    /// Whether a receipt naming the contract hash `hash` (and the lock
    /// digest `lock`, when it names one) ran under this gate: what a failing
    /// receipt must name to be a failure of the commit, as a passing one
    /// must for `sykli verify` to accept it. One that names no contract
    /// never did.
    pub fn ran(&self, hash: Option<&str>, lock: Option<&str>) -> bool {
        hash.is_some_and(|hash| contract_hash(&self.contract).as_deref() == Some(hash))
            && lock.is_none_or(|lock| self.lock.as_deref().map(sha256).as_deref() == Some(lock))
    }

    /// Its file under `base`, `<base>/<digest>/sykli.json`, written there
    /// (and its lock beside it) unless it already is: the same bytes
    /// whenever it is written.
    pub fn write(&self, base: &Path) -> Result<PathBuf, String> {
        let dir = base.join(&self.digest);
        std::fs::create_dir_all(&dir)
            .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
        let contract = dir.join("sykli.json");
        put(&contract, Some(&self.contract))?;
        put(&dir.join("sykli.lock"), self.lock.as_deref())?;
        Ok(contract)
    }

    /// Its file under `base` when it is there as derived, lock and all.
    fn found(&self, base: &Path) -> Option<PathBuf> {
        let contract = base.join(&self.digest).join("sykli.json");
        let lock = std::fs::read(contract.with_file_name("sykli.lock")).ok();
        (std::fs::read(&contract).ok().as_deref() == Some(&self.contract[..]) && lock == self.lock)
            .then_some(contract)
    }
}

/// Where derived gates are written for sykli, which reads a contract, and
/// the `sykli.lock` beside it, from their files (it takes no contract on
/// stdin). A gate a run that outlives this ohjaa names (`ohjaa gate`, an
/// integrated validation) is kept in ohjaa's state, `$XDG_STATE_HOME/
/// ohjaa/derived/<digest>/`, beside the clones' validation folders, and,
/// for a commit with no contract of its own, what `sykli init` wrote for
/// its tree beside it (`inferred/<tree>/`), so the tree's gate is never
/// asked of sykli again. Every other gate sykli only reads now (to verify a
/// receipt, judge a push, say what applies to a commit, print a plan) is
/// read from there when it is kept, else written into a folder of this
/// process's own under `$TMPDIR`, removed when this is dropped (and so is
/// the scratch checkout `sykli init` is asked in): the marks, the report and
/// `ohjaa gate --print` write nothing that stays, and never into the config
/// home.
#[derive(Debug, Default)]
pub struct Gates {
    /// `$XDG_STATE_HOME/ohjaa/derived`; `None` when there is no state home.
    kept: Option<PathBuf>,
    /// `$XDG_STATE_HOME/ohjaa/inferred`: what `sykli init` wrote for each
    /// tree a run was judged by.
    trees: Option<PathBuf>,
    /// This process's own folder, made the first time a gate is written
    /// there.
    scratch: std::cell::RefCell<Option<PathBuf>>,
    /// What `sykli init` wrote for each tree this process asked it about,
    /// or why it wrote nothing: each tree is asked once.
    inferred: std::cell::RefCell<std::collections::HashMap<String, Result<Own, String>>>,
    /// `sykli init --help`, asked once: whether it takes `--contract`, and
    /// the repository's facts.
    init_help: std::cell::OnceCell<Result<(bool, bool), String>>,
}

impl Gates {
    pub fn new(kept: Option<PathBuf>) -> Gates {
        Gates {
            trees: kept
                .as_deref()
                .and_then(Path::parent)
                .map(|state| state.join("inferred")),
            kept,
            scratch: Default::default(),
            inferred: Default::default(),
            init_help: Default::default(),
        }
    }

    /// Kept under this machine's ohjaa state.
    pub fn of_state() -> Gates {
        Gates::new(
            crate::knowledge::state_home()
                .ok()
                .map(|state| state.join("ohjaa").join("derived")),
        )
    }

    /// Where a run keeps `derived`, and whether it is there now; `None`
    /// without a state home. Nothing is written.
    pub fn kept_at(&self, derived: &Derived) -> Option<(PathBuf, bool)> {
        let kept = self.kept.as_deref()?;
        let found = derived.found(kept);
        Some((
            kept.join(&derived.digest).join("sykli.json"),
            found.is_some(),
        ))
    }

    /// For a run that outlives this ohjaa: kept in the state, and, for a
    /// commit with no contract of its own, what `sykli init` wrote for its
    /// tree beside it.
    pub fn keep(&self, derived: &Derived) -> Result<PathBuf, String> {
        let kept = self
            .kept
            .as_deref()
            .ok_or("there is no state directory to keep the gate in (XDG_STATE_HOME, HOME)")?;
        let path = derived.write(kept)?;
        if let (Some(inferred), Some(trees)) = (&derived.inferred, self.trees.as_deref()) {
            keep_inferred(trees, inferred)?;
        }
        Ok(path)
    }

    /// For sykli to read now: where a run kept it, else in this process's
    /// own folder.
    pub fn read(&self, derived: &Derived) -> Result<PathBuf, String> {
        if let Some(found) = self.kept.as_deref().and_then(|kept| derived.found(kept)) {
            return Ok(found);
        }
        derived.write(&self.scratch_folder()?)
    }

    /// This process's own folder, made the first time it is needed.
    fn scratch_folder(&self) -> Result<PathBuf, String> {
        let mut scratch = self.scratch.borrow_mut();
        if scratch.is_none() {
            *scratch = Some(private_folder()?);
        }
        Ok(scratch.clone().expect("just made"))
    }

    /// What `sykli init` writes for `tree`, of the clone whose common git
    /// dir is `common`: as a run kept it, in the state (no process); else
    /// asked of `sykli` (each call at most `limit`) in a scratch checkout of
    /// the tree, once per process, and kept only by [`Gates::keep`]. The
    /// scratch checkout is a repository of its own under this process's
    /// folder, borrowing the clone's objects (`objects/info/alternates`):
    /// two git processes (`init`, `read-tree -u`) and nothing written into
    /// the clone, the config home or the state.
    pub fn inferred(
        &self,
        common: &Path,
        tree: &str,
        sykli: &Path,
        limit: std::time::Duration,
    ) -> Result<Own, String> {
        if tree.is_empty() || !tree.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("{tree:?} is not a tree id"));
        }
        if let Some(own) = self
            .trees
            .as_deref()
            .and_then(|trees| read_inferred(trees, tree))
        {
            return Ok(own);
        }
        if let Some(known) = self.inferred.borrow().get(tree) {
            return known.clone();
        }
        let asked = self.ask_init(common, tree, sykli, limit);
        self.inferred
            .borrow_mut()
            .insert(tree.to_owned(), asked.clone());
        asked
    }

    fn ask_init(
        &self,
        common: &Path,
        tree: &str,
        sykli: &Path,
        limit: std::time::Duration,
    ) -> Result<Own, String> {
        let (contract, facts) = self
            .init_help
            .get_or_init(|| init_help(sykli, limit))
            .clone()?;
        if !contract {
            return Err(
                "the sykli here cannot write a contract outside a repository (`sykli init \
                 --help` names no --contract)"
                    .to_owned(),
            );
        }
        let dir = self.scratch_folder()?.join(format!("init-{tree}"));
        let asked = init_in(&dir, common, tree, (sykli, facts), limit);
        let _ = std::fs::remove_dir_all(&dir);
        asked
    }
}

/// `sykli init --help`: whether it names `--contract`, and `--repo-root`
/// with `--git-common-dir`.
fn init_help(sykli: &Path, limit: std::time::Duration) -> Result<(bool, bool), String> {
    let mut command = Command::new(sykli);
    command.args(["init", "--help"]);
    let output = process::run(&mut command, None, limit)
        .map_err(|error| format!("sykli could not be asked: {error:#}"))?;
    let help = format!(
        "{}{}",
        output.stdout_text(),
        String::from_utf8_lossy(&output.stderr)
    );
    let names = |flag| process::names_flag(&help, flag);
    Ok((
        names("--contract"),
        names("--repo-root") && names("--git-common-dir"),
    ))
}

/// Ask `sykli init` for the contract of `tree`, in a scratch checkout of it
/// at `dir`: a repository of its own (`git init`, in the clone's object
/// format) whose objects are the clone's (`objects/info/alternates`, read
/// only), the tree read into its index and work tree (`read-tree -u`), and
/// nothing else in it; then `sykli init --contract <dir>/gate/sykli.json`,
/// which writes the contract and its lock there and nothing in the
/// checkout. No hook runs, nothing is fetched (`GIT_NO_LAZY_FETCH`), and no
/// repository a hook's environment names is touched.
fn init_in(
    dir: &Path,
    common: &Path,
    tree: &str,
    (sykli, facts): (&Path, bool),
    limit: std::time::Duration,
) -> Result<Own, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let work = dir.join("tree");
    let gate = dir.join("gate").join("sykli.json");
    let quiet = |command: &mut Command| {
        for var in crate::gitconfig::REPO_ENV
            .into_iter()
            .chain(["GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"])
        {
            command.env_remove(var);
        }
        command.env_remove(crate::sessions::SESSION_ENV);
    };
    let git = |args: &[&std::ffi::OsStr]| -> Result<(), String> {
        let mut command = Command::new("git");
        command
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(args)
            .env("GIT_NO_LAZY_FETCH", "1")
            .env("LC_ALL", "C");
        quiet(&mut command);
        let output = process::run(&mut command, None, process::LOCAL)
            .map_err(|error| format!("git could not be asked: {error:#}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(output.failure_detail())
        }
    };
    let format = if tree.len() == 64 { "sha256" } else { "sha1" };
    let objects = std::fs::canonicalize(common)
        .map_err(|error| format!("cannot read {}: {error}", common.display()))?
        .join("objects");
    git(&[
        "init".as_ref(),
        "-q".as_ref(),
        format!("--object-format={format}").as_ref(),
        work.as_os_str(),
    ])
    .map_err(|why| format!("no scratch checkout of its tree could be made: {why}"))?;
    let mut borrowed = objects.as_os_str().as_bytes().to_vec();
    borrowed.push(b'\n');
    let alternates = work.join(".git/objects/info/alternates");
    std::fs::write(&alternates, borrowed)
        .map_err(|error| format!("cannot write {}: {error}", alternates.display()))?;
    git(&[
        "-C".as_ref(),
        work.as_os_str(),
        "read-tree".as_ref(),
        "-u".as_ref(),
        "--reset".as_ref(),
        tree.as_ref(),
    ])
    .map_err(|why| format!("its tree cannot be checked out for sykli init: {why}"))?;
    if let Some(folder) = gate.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("cannot create {}: {error}", folder.display()))?;
    }
    let mut init = Command::new(sykli);
    init.args(["init", "--contract"]).arg(&gate);
    if facts {
        init.arg("--repo-root")
            .arg(&work)
            .arg("--git-common-dir")
            .arg(work.join(".git"));
    }
    init.current_dir(&work);
    quiet(&mut init);
    let output = process::run(&mut init, None, limit)
        .map_err(|error| format!("sykli init could not run: {error:#}"))?;
    if !output.status.success() {
        return Err(format!(
            "sykli init exited {}: {}",
            output.status.code().unwrap_or(1),
            output.failure_detail()
        ));
    }
    let contract = std::fs::read(&gate)
        .map_err(|error| format!("sykli init wrote no {} ({error})", gate.display()))?;
    Ok(Own {
        contract,
        lock: std::fs::read(gate.with_file_name("sykli.lock")).ok(),
    })
}

/// What a run kept of `sykli init`'s contract for `tree`, in `trees`.
fn read_inferred(trees: &Path, tree: &str) -> Option<Own> {
    let dir = trees.join(tree);
    let contract = std::fs::read(dir.join("sykli.json")).ok()?;
    Some(Own {
        contract,
        lock: std::fs::read(dir.join("sykli.lock")).ok(),
    })
}

/// Keep what `sykli init` wrote for a tree, `<trees>/<tree>/sykli.json` and
/// the `sykli.lock` beside it: made whole aside, then moved into place, so
/// a reader finds both or neither. The first one kept stays.
fn keep_inferred(trees: &Path, inferred: &Inferred) -> Result<(), String> {
    let dir = trees.join(&inferred.tree);
    if dir.join("sykli.json").is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(trees)
        .map_err(|error| format!("cannot create {}: {error}", trees.display()))?;
    let aside = trees.join(format!(".{}.ohjaa-{}", inferred.tree, std::process::id()));
    let _ = std::fs::remove_dir_all(&aside);
    let made = std::fs::create_dir(&aside)
        .and_then(|()| std::fs::write(aside.join("sykli.json"), &inferred.init.contract))
        .and_then(|()| match &inferred.init.lock {
            Some(lock) => std::fs::write(aside.join("sykli.lock"), lock),
            None => Ok(()),
        })
        .and_then(|()| std::fs::rename(&aside, &dir));
    match made {
        Ok(()) => Ok(()),
        // Another ohjaa kept it first.
        Err(_) if dir.join("sykli.json").is_file() => {
            let _ = std::fs::remove_dir_all(&aside);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&aside);
            Err(format!("cannot keep {}: {error}", dir.display()))
        }
    }
}

impl Drop for Gates {
    fn drop(&mut self) {
        if let Some(scratch) = self.scratch.get_mut().take() {
            let _ = std::fs::remove_dir_all(scratch);
        }
    }
}

/// A new folder under `$TMPDIR` that only this process writes in.
fn private_folder() -> Result<PathBuf, String> {
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tmp = std::env::temp_dir();
    for _ in 0..100 {
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = tmp.join(format!("ohjaa-gates-{}-{n}", std::process::id()));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("cannot create {}: {error}", dir.display())),
        }
    }
    Err(format!(
        "cannot make a folder of its own in {}",
        tmp.display()
    ))
}

/// Make `path` hold exactly `bytes` (no file for `None`), without a moment
/// where it is half written; untouched when it already does.
fn put(path: &Path, bytes: Option<&[u8]>) -> Result<(), String> {
    let now = std::fs::read(path).ok();
    if now.as_deref() == bytes {
        return Ok(());
    }
    let Some(bytes) = bytes else {
        return std::fs::remove_file(path)
            .map_err(|error| format!("cannot remove {}: {error}", path.display()));
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.ohjaa-{}", std::process::id()));
    std::fs::write(&temp, bytes)
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("cannot write {}: {error}", path.display())
        })
}

/// The contract one commit of an entry's branch is judged by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAt {
    /// Derived from the commit: from its own `sykli.json`, or, with no
    /// contract of its own, from what `sykli init` writes for its tree.
    Derived(Box<Derived>),
    /// None: why.
    Missing(String),
}

impl GateAt {
    /// For `commit`, which holds `holds`, with the entry's `overlay`; a
    /// commit with no contract of its own asks `infer` what `sykli init`
    /// writes for its tree.
    pub fn of(
        overlay: &Overlay,
        commit: &str,
        holds: Holds,
        infer: impl FnOnce(&str) -> Result<Own, String>,
    ) -> GateAt {
        match holds {
            Holds::Own(own) => GateAt::Derived(Box::new(derive(&own, overlay))),
            Holds::Emitter => GateAt::Missing(format!(
                "no sykli contract (sykli.json) in {}: its contract is a sykli.rs, which no gate \
                 is derived from",
                short(commit)
            )),
            Holds::Neither { tree } => match infer(&tree) {
                Ok(init) => {
                    GateAt::Derived(Box::new(derive_inferred(Inferred { tree, init }, overlay)))
                }
                Err(why) => GateAt::Missing(format!(
                    "no sykli contract (sykli.json) in {}, and sykli init writes none for its \
                     tree: {why}",
                    short(commit)
                )),
            },
        }
    }

    /// The file sykli is handed: a derived gate where `gates` keeps it, for
    /// a run that outlives this ohjaa (`keep`), else where sykli reads it
    /// now.
    pub fn contract(&self, gates: &Gates, keep: bool) -> Result<PathBuf, String> {
        match self {
            GateAt::Derived(derived) if keep => gates.keep(derived),
            GateAt::Derived(derived) => gates.read(derived),
            GateAt::Missing(why) => Err(why.clone()),
        }
    }

    /// Where the gate comes from, for people, the contract being `at` (`at
    /// 0c9d3b1`, `in this checkout`): `derived from the repository's own
    /// sykli.json at 0c9d3b1; [cargo_env] given to fmt, test`.
    pub fn describe(&self, at: &str) -> String {
        match self {
            GateAt::Derived(derived) => {
                let given = if !derived.given.is_empty() {
                    format!("; [cargo_env] given to {}", derived.given.join(", "))
                } else if let Some(why) = &derived.unread {
                    format!("; no [cargo_env] line given: it is {why}")
                } else if derived.cargo.is_empty() {
                    "; it runs no cargo, so it is that contract unchanged".to_owned()
                } else {
                    "; no cargo task is given a [cargo_env] line, so it is that contract \
                     unchanged"
                        .to_owned()
                };
                let pinned = match (derived.pinned, &derived.inferred) {
                    (Pinned::Yes, _) => "",
                    (Pinned::NoLock, _) => "; it has no sykli.lock",
                    (Pinned::No, None) => {
                        "; its sykli.lock does not pin its sykli.json, so sykli refuses to run it \
                         (sykli lock it in the repository)"
                    }
                    (Pinned::No, Some(_)) => {
                        "; the sykli.lock sykli init wrote does not pin it, so sykli refuses to run \
                         it"
                    }
                };
                let from = match &derived.inferred {
                    None => format!("derived from the repository's own sykli.json {at}"),
                    Some(inferred) => format!(
                        "derived from what sykli init writes for the tree {at} ({}), which has no \
                         sykli.json of its own",
                        short(&inferred.tree)
                    ),
                };
                format!("{from}{given}{pinned}")
            }
            GateAt::Missing(why) => format!("no gate: {why}"),
        }
    }
}

/// The contract a receipt that proved a commit ran under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Under {
    /// The gate derived from the commit (from its own `sykli.json`, or what
    /// `sykli init` writes for its tree).
    Derived,
    /// The commit's own `sykli.json`, as it is (CI's `sykli run sykli.json`).
    Own,
}

impl Under {
    /// For tools: `derived` or `own` (`stored`, an entry's copy, is no
    /// longer given: no copy is read).
    pub fn word(self) -> &'static str {
        match self {
            Under::Derived => "derived",
            Under::Own => "own",
        }
    }
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

/// An object in git's store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub oid: String,
    pub kind: String,
    pub bytes: Vec<u8>,
}

/// A commit, as the marks show one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub oid: String,
    pub tree: String,
    /// Its first parent; `None` for a root (or where a shallow clone ends).
    pub parent: Option<String>,
    pub author: String,
    /// Author time, unix seconds.
    pub authored: i64,
    /// Its message's first paragraph, on one line (git's `%s`).
    pub subject: String,
}

/// A commit object's text, read as `git log` would show it with
/// `%T`, the first parent, `%an`, `%at` and `%s`.
pub fn parse_commit(oid: &str, bytes: &[u8]) -> Option<Commit> {
    let text = String::from_utf8_lossy(bytes);
    let (headers, message) = text.split_once("\n\n").unwrap_or((&text, ""));
    let (mut tree, mut parent) = (None, None);
    let (mut author, mut authored) = (String::new(), 0);
    for line in headers.lines() {
        if let Some(oid) = line.strip_prefix("tree ") {
            tree.get_or_insert_with(|| oid.trim().to_owned());
        } else if let Some(oid) = line.strip_prefix("parent ") {
            parent.get_or_insert_with(|| oid.trim().to_owned());
        } else if let Some(who) = line.strip_prefix("author ") {
            // `Name <email> 1700000000 +0200`
            let (name, rest) = who.split_once('<').unwrap_or((who, ""));
            author = name.trim_end().to_owned();
            authored = rest
                .rsplit(' ')
                .nth(1)
                .and_then(|seconds| seconds.parse().ok())
                .unwrap_or(0);
        }
    }
    let subject = message
        .trim_start_matches('\n')
        .split("\n\n")
        .next()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(Commit {
        oid: oid.to_owned(),
        tree: tree?,
        parent,
        author,
        authored,
        subject,
    })
}

/// One `git cat-file --batch`, asked one object at a time: every commit,
/// tree and file one ohjaa command needs of a clone, from one git process.
/// Never fetches (`GIT_NO_LAZY_FETCH`): an object a partial clone lacks is
/// missing, not asked of the network.
#[derive(Debug)]
pub struct Objects {
    talk: process::Conversation,
    /// Set once an answer could not be read: nothing more is asked.
    broken: Option<String>,
}

impl Objects {
    /// In the clone whose common git dir is `common`.
    pub fn of_clone(common: &Path) -> Result<Objects, String> {
        let mut command = Command::new("git");
        command.arg("--git-dir").arg(common);
        Objects::start(command)
    }

    /// In the checkout at `top`, whose `HEAD` is then this checkout's.
    pub fn in_checkout(top: &Path) -> Result<Objects, String> {
        let mut command = Command::new("git");
        command.arg("-C").arg(top);
        Objects::start(command)
    }

    fn start(mut command: Command) -> Result<Objects, String> {
        command
            .args(["cat-file", "--batch"])
            .env("GIT_NO_LAZY_FETCH", "1");
        for var in crate::gitconfig::REPO_ENV {
            command.env_remove(var);
        }
        let talk = process::Conversation::start(&mut command, process::LOCAL)
            .map_err(|error| format!("git could not be asked: {error:#}"))?;
        Ok(Objects { talk, broken: None })
    }

    /// The object `name` names (`<rev>`, `<rev>:<path>`, `<rev>^{tree}`);
    /// `None` when there is none.
    pub fn get(&mut self, name: &str) -> Result<Option<Object>, String> {
        if name.is_empty() || name.contains(['\n', '\r']) {
            return Err(format!("{name:?} is not an object name"));
        }
        if let Some(why) = &self.broken {
            return Err(why.clone());
        }
        let answer = self.question(name);
        self.talk.answered();
        if let Err(why) = &answer {
            self.broken = Some(why.clone());
        }
        answer
    }

    fn question(&mut self, name: &str) -> Result<Option<Object>, String> {
        let cut = || "git cat-file stopped before it answered (out of time, or gone)".to_owned();
        let input = self.talk.ask().ok_or_else(cut)?;
        input
            .write_all(format!("{name}\n").as_bytes())
            .and_then(|()| input.flush())
            .map_err(|_| cut())?;
        let output = self.talk.output();
        let mut header = Vec::new();
        output.read_until(b'\n', &mut header).map_err(|_| cut())?;
        if header.pop() != Some(b'\n') {
            return Err(cut());
        }
        let header = String::from_utf8_lossy(&header).into_owned();
        if header.ends_with(" missing") || header.ends_with(" ambiguous") {
            return Ok(None);
        }
        let fields: Vec<&str> = header.split(' ').collect();
        let [oid, kind, size] = fields[..] else {
            return Err(format!("git cat-file answered {header:?}"));
        };
        let size: usize = size
            .parse()
            .map_err(|_| format!("git cat-file answered {header:?}"))?;
        let mut bytes = vec![0; size + 1];
        output.read_exact(&mut bytes).map_err(|_| cut())?;
        if bytes.pop() != Some(b'\n') {
            return Err(format!("git cat-file's answer for {name} is cut short"));
        }
        Ok(Some(Object {
            oid: oid.to_owned(),
            kind: kind.to_owned(),
            bytes,
        }))
    }

    /// The commit `name` names; `None` when there is none.
    pub fn commit(&mut self, name: &str) -> Result<Option<Commit>, String> {
        let Some(object) = self.get(name)? else {
            return Ok(None);
        };
        if object.kind != "commit" {
            return Err(format!("{name} is a {}, not a commit", object.kind));
        }
        parse_commit(&object.oid, &object.bytes)
            .map(Some)
            .ok_or_else(|| format!("commit {} names no tree", object.oid))
    }

    /// What `commit` holds of its own contract, read by the entries of its
    /// top tree: its `sykli.json` and `sykli.lock`; else a `sykli.rs`; else
    /// nothing, and its tree. Anything else is said, never read as no
    /// contract: a commit this clone does not have, a `sykli.json` (or
    /// `sykli.lock`) that is a symbolic link or no file, or one whose file a
    /// partial clone lacks.
    pub fn holds_at(&mut self, commit: &str) -> Result<Holds, String> {
        let Some(tree) = self.get(&format!("{commit}^{{tree}}"))? else {
            return Err(format!("{} is not in this clone", short(commit)));
        };
        let entries = tree_entries(&tree.bytes, tree.oid.len() / 2)?;
        if let Some(contract) = self.file_in(commit, &entries, "sykli.json")? {
            let lock = self.file_in(commit, &entries, "sykli.lock")?;
            return Ok(Holds::Own(Own { contract, lock }));
        }
        if entries.iter().any(|entry| entry.name == "sykli.rs") {
            return Ok(Holds::Emitter);
        }
        Ok(Holds::Neither { tree: tree.oid })
    }

    /// The file `name` of a top tree's `entries`, at `commit`.
    fn file_in(
        &mut self,
        commit: &str,
        entries: &[TreeEntry],
        name: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(entry) = entries.iter().find(|entry| entry.name == name) else {
            return Ok(None);
        };
        let at = short(commit);
        match entry.mode.as_str() {
            "100644" | "100755" => {}
            "120000" => {
                return Err(format!(
                    "{name} at {at} is a symbolic link; a gate is judged by a file of that name"
                ))
            }
            mode => return Err(format!("{name} at {at} is not a file (mode {mode})")),
        }
        match self.get(&entry.oid)? {
            Some(blob) if blob.kind == "blob" => Ok(Some(blob.bytes)),
            _ => Err(format!(
                "{name} at {at} is not in this clone (a partial clone without it: ohjaa never \
                 fetches)"
            )),
        }
    }
}

/// One entry of a tree object: `<mode> <name>\0<object id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    mode: String,
    name: String,
    oid: String,
}

/// A tree object's entries, its object ids `oid_bytes` long (20 for SHA-1,
/// 32 for SHA-256).
fn tree_entries(mut bytes: &[u8], oid_bytes: usize) -> Result<Vec<TreeEntry>, String> {
    let cut = || "a tree object git gave is cut short".to_owned();
    let mut entries = Vec::new();
    while !bytes.is_empty() {
        let space = bytes
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(cut)?;
        let nul = bytes.iter().position(|byte| *byte == 0).ok_or_else(cut)?;
        if nul < space || bytes.len() < nul + 1 + oid_bytes {
            return Err(cut());
        }
        entries.push(TreeEntry {
            mode: String::from_utf8_lossy(&bytes[..space]).into_owned(),
            name: String::from_utf8_lossy(&bytes[space + 1..nul]).into_owned(),
            oid: bytes[nul + 1..nul + 1 + oid_bytes]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        });
        bytes = &bytes[nul + 1 + oid_bytes..];
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_the_standards() {
        use sha2::Digest as _;
        let mut long = Vec::new();
        for n in 0..1000u32 {
            long.extend_from_slice(&n.to_le_bytes());
        }
        for n in 0..=200usize {
            let input: Vec<u8> = (0..n).map(|at| (at * 31 + 7) as u8).collect();
            assert_eq!(
                sha256(&input),
                format!("{:x}", sha2::Sha256::digest(&input)),
                "{n} bytes"
            );
        }
        for input in [
            &b""[..],
            b"abc",
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            &[0u8; 55],
            &[0u8; 56],
            &[7u8; 64],
            &[1u8; 119],
            &long,
        ] {
            assert_eq!(
                sha256(input),
                format!("{:x}", sha2::Sha256::digest(input)),
                "{} bytes",
                input.len()
            );
        }
    }

    /// sykli's hash of a contract is that of its JSON with sorted keys and
    /// no blanks, whatever the file's layout; and a lock is read as sykli
    /// reads it.
    #[test]
    fn a_contract_is_hashed_as_sykli_hashes_it() {
        let pretty = b"{\n  \"tasks\": [],\n  \"schema\": \"sykli-contract.v1\"\n}\n";
        let canonical = br#"{"schema":"sykli-contract.v1","tasks":[]}"#;
        assert_eq!(contract_hash(pretty), Some(sha256(canonical)));
        assert_eq!(contract_hash(b"not json"), None);
        let v2 = br#"{"schema":"sykli-lock.v2","contracts":{"sykli.json":"ab","x.json":"cd"}}"#;
        assert_eq!(pinned_in(v2, "sykli.json").as_deref(), Some("ab"));
        assert_eq!(pinned_in(v2, "y.json"), None);
        let v1 = br#"{"schema":"sykli-lock.v1","contract_hash":"ef"}"#;
        assert_eq!(pinned_in(v1, "sykli.json").as_deref(), Some("ef"));
        assert_eq!(pinned_in(b"{}", "sykli.json"), None);
        let fresh = fresh_lock("12");
        assert_eq!(pinned_in(&fresh, "sykli.json").as_deref(), Some("12"));
    }

    const GATE: &str = r#"{
  "schema": "sykli-contract.v1",
  "tasks": [
    {"name": "fmt", "run": "cargo fmt --check", "inputs": ["src/main.rs"]},
    {"name": "lint", "run": "sh -n install.sh", "inputs": ["install.sh"]},
    {"name": "own", "run": "cargo test", "env": {"CARGO_TARGET_DIR": "t"}}
  ]
}
"#;

    fn locked(contract: &str) -> Own {
        Own {
            contract: contract.as_bytes().to_vec(),
            lock: Some(fresh_lock(&contract_hash(contract.as_bytes()).unwrap())),
        }
    }

    /// The same commit gives the same gate, byte for byte, whenever and
    /// wherever it is derived; the lines go to the cargo tasks that say
    /// nothing of where they build, and nowhere else.
    #[test]
    fn a_gate_is_derived_deterministically_and_only_cargo_tasks_get_lines() {
        let own = locked(GATE);
        let one = derive(&own, &Overlay::standard());
        assert_eq!(one, derive(&own.clone(), &Overlay::standard()));
        assert_eq!(one.given, ["fmt"]);
        assert_eq!(one.pinned, Pinned::Yes);
        let gate: serde_json::Value = serde_json::from_slice(&one.contract).unwrap();
        let tasks = gate["tasks"].as_array().unwrap();
        assert_eq!(
            tasks[0]["env"],
            serde_json::json!({
                "CARGO_TARGET_DIR": "${worktree_cache}/cargo-target",
                "CARGO_BUILD_BUILD_DIR": "${worktree_cache}/cargo-build"
            })
        );
        assert_eq!(tasks[1].get("env"), None, "no cargo, no line");
        assert_eq!(
            tasks[2]["env"],
            serde_json::json!({"CARGO_TARGET_DIR": "t"}),
            "a task that says its target dir keeps it"
        );
        // Written as sykli canonicalises it, and locked to it.
        assert_eq!(contract_hash(&one.contract), Some(sha256(&one.contract)));
        assert_eq!(
            pinned_in(one.lock.as_deref().unwrap(), "sykli.json"),
            Some(sha256(&one.contract))
        );
        // The repository's own contract stays known: a receipt of a run
        // under it proves the commit too.
        let own_id = one.own.clone().unwrap();
        assert_eq!(Some(own_id.hash.clone()), contract_hash(GATE.as_bytes()));
        assert!(own_id.ran(
            Some(&own_id.hash),
            own.lock.as_deref().map(sha256).as_deref()
        ));
        // A receipt that names no lock digest (an older sykli wrote none)
        // ran under it all the same: `sykli verify` checks its lock.
        assert!(own_id.ran(Some(&own_id.hash), None));
        assert!(!own_id.ran(Some(&own_id.hash), Some("another lock")));
        assert!(!own_id.ran(Some("another contract"), None));
        assert!(!own_id.ran(None, None));
    }

    /// A gate sykli reads now is read from where a run kept it, in the
    /// state, when it is there; else it is written into a folder of this
    /// process's own, which goes when its `Gates` does. Only `keep` writes
    /// into the state, and writing it again changes nothing.
    #[test]
    fn a_gate_is_kept_in_the_state_only_for_a_run_and_read_from_a_private_folder_else() {
        let base = std::env::temp_dir().join(format!("ohjaa-gates-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let state = base.join("state/ohjaa/derived");
        let one = derive(&locked(GATE), &Overlay::standard());
        let gates = Gates::new(Some(state.clone()));
        let read = gates.read(&one).unwrap();
        assert!(!read.starts_with(&state), "{}", read.display());
        assert!(!state.exists(), "reading keeps nothing");
        assert_eq!(std::fs::read(&read).unwrap(), one.contract);
        assert_eq!(
            std::fs::read(read.with_file_name("sykli.lock")).ok(),
            one.lock
        );
        let kept = gates.keep(&one).unwrap();
        assert_eq!(kept, state.join(&one.digest).join("sykli.json"));
        assert_eq!(std::fs::read(&kept).unwrap(), one.contract);
        assert_eq!(gates.keep(&one).unwrap(), kept);
        let scratch = read.parent().unwrap().parent().unwrap().to_path_buf();
        drop(gates);
        assert!(!scratch.exists(), "the private folder goes with it");
        // Kept: read from the state, nothing written anywhere.
        let again = Gates::new(Some(state.clone()));
        assert_eq!(again.read(&one).unwrap(), kept);
        assert!(again.scratch.borrow().is_none(), "no private folder made");
        // A gate written over in the state is not read from there.
        std::fs::write(&kept, "{}").unwrap();
        let other = again.read(&one).unwrap();
        assert_ne!(other, kept);
        assert_eq!(std::fs::read(&other).unwrap(), one.contract);
        // No state at all: read still works, keep says why not.
        let nowhere = Gates::new(None);
        assert!(nowhere.read(&one).is_ok());
        assert!(nowhere.keep(&one).is_err());
        drop((again, nowhere));
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// G1c: a tree with no contract of its own is asked of `sykli init` in
    /// a scratch checkout of exactly that tree (a repository of its own under
    /// this process's folder, borrowing the clone's objects): what the clone's
    /// work tree holds beside it never reaches it. Each tree is asked once
    /// per process; nothing is kept but by `keep`, which keeps it by tree,
    /// and every later reader takes it from there without asking sykli.
    #[test]
    fn a_tree_is_asked_of_sykli_init_in_a_scratch_checkout_once_and_kept_by_tree() {
        let base = std::env::temp_dir().join(format!("ohjaa-inferred-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let clone = base.join("clone");
        std::fs::create_dir_all(&clone).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let output = Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@x", "-C"])
                .arg(dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&clone, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(clone.join("src")).unwrap();
        std::fs::write(clone.join("src/a.rs"), "x\n").unwrap();
        std::fs::write(clone.join("README"), "x\n").unwrap();
        git(&clone, &["add", "-A"]);
        git(&clone, &["commit", "-q", "-m", "one"]);
        std::fs::write(clone.join("untracked.rs"), "x\n").unwrap();
        let tree = git(&clone, &["rev-parse", "HEAD^{tree}"]);
        // A sykli whose init writes the files git tracks where it runs.
        let sykli = base.join("sykli");
        std::fs::write(
            &sykli,
            format!(
                "#!/bin/sh\necho \"$PWD|$*\" >> '{}'\n\
                 [ \"$2\" = --help ] && {{ echo '      --contract <PATH>  outside'; exit 0; }}\n\
                 printf '{{\"inputs\":\"%s\"}}' \"$(git ls-files | paste -sd, -)\" > \"$3\"\n\
                 echo lock > \"$(dirname \"$3\")/sykli.lock\"\n",
                base.join("calls").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&sykli, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let state = base.join("state/ohjaa/derived");
        let limit = std::time::Duration::from_secs(30);
        let gates = Gates::new(Some(state.clone()));
        let common = clone.join(".git");
        let own = gates.inferred(&common, &tree, &sykli, limit).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&own.contract),
            "{\"inputs\":\"README,src/a.rs\"}",
            "exactly the tree"
        );
        assert_eq!(own.lock.as_deref(), Some(&b"lock\n"[..]));
        assert_eq!(gates.inferred(&common, &tree, &sykli, limit).unwrap(), own);
        let calls = std::fs::read_to_string(base.join("calls")).unwrap();
        assert_eq!(calls.matches("|init --contract ").count(), 1, "{calls}");
        assert!(!state.parent().unwrap().join("inferred").exists());
        assert!(gates
            .inferred(&common, "not a tree", &sykli, limit)
            .is_err());
        let derived = derive_inferred(
            Inferred {
                tree: tree.clone(),
                init: own.clone(),
            },
            &Overlay::standard(),
        );
        assert_eq!(
            derived.own, None,
            "no contract of its own to verify against"
        );
        gates.keep(&derived).unwrap();
        let kept = state.parent().unwrap().join("inferred").join(&tree);
        assert_eq!(
            std::fs::read(kept.join("sykli.json")).unwrap(),
            own.contract
        );
        let scratch = gates.scratch.borrow().clone().unwrap();
        drop(gates);
        assert!(!scratch.exists(), "the scratch checkout went with it");
        // Kept: another process reads it, never asking sykli.
        let again = Gates::new(Some(state));
        let gone = base.join("no-sykli");
        assert_eq!(again.inferred(&common, &tree, &gone, limit).unwrap(), own);
        assert!(again.scratch.borrow().is_none());
        drop(again);
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// Any change to the commit's contract or lock, or to the entry's
    /// lines, is another gate, in another folder.
    #[test]
    fn a_contract_changed_at_the_commit_changes_the_derived_digest() {
        let base = derive(&locked(GATE), &Overlay::standard());
        let more = GATE.replace("\"src/main.rs\"]", "\"src/main.rs\", \"src/new.rs\"]");
        let changed = derive(&locked(&more), &Overlay::standard());
        assert_ne!(base.digest, changed.digest);
        assert_ne!(base.contract, changed.contract);
        let unlocked = derive(
            &Own {
                contract: GATE.as_bytes().to_vec(),
                lock: None,
            },
            &Overlay::standard(),
        );
        assert_ne!(base.digest, unlocked.digest);
        assert_eq!(unlocked.pinned, Pinned::NoLock);
        assert_eq!(unlocked.lock, None, "what the repository did not lock");
        let none = derive(&locked(GATE), &Overlay::default());
        assert_ne!(base.digest, none.digest);
        assert!(none.given.is_empty());
        assert_eq!(none.contract, GATE.as_bytes(), "no line: as it is");
        assert_eq!(none.own, None, "it is the repository's own contract");
    }

    /// A lock that does not pin the contract (changed without `sykli lock`)
    /// is kept as it is: sykli refuses the gate as it refuses the
    /// repository's own. Never locked by ohjaa.
    #[test]
    fn a_lock_that_does_not_pin_its_contract_never_pins_the_gate() {
        let stale = locked(&GATE.replace("fmt --check", "fmt"));
        let own = Own {
            contract: GATE.as_bytes().to_vec(),
            lock: stale.lock.clone(),
        };
        let derived = derive(&own, &Overlay::standard());
        assert_eq!(derived.pinned, Pinned::No);
        // Locked to the repository's own contract, which the gate given
        // lines is not: sykli refuses it, whatever that lock pinned.
        assert_eq!(
            pinned_in(derived.lock.as_deref().unwrap(), "sykli.json"),
            contract_hash(GATE.as_bytes())
        );
        assert_ne!(
            pinned_in(derived.lock.as_deref().unwrap(), "sykli.json"),
            Some(sha256(&derived.contract))
        );
        // Even a lock that pins the gate itself, not the repository's
        // contract, does not make sykli accept the gate.
        let given = derive(&locked(GATE), &Overlay::standard());
        let sneaky = Own {
            contract: GATE.as_bytes().to_vec(),
            lock: Some(fresh_lock(&sha256(&given.contract))),
        };
        let derived = derive(&sneaky, &Overlay::standard());
        assert_eq!(derived.pinned, Pinned::No);
        assert_ne!(
            pinned_in(derived.lock.as_deref().unwrap(), "sykli.json"),
            Some(sha256(&derived.contract))
        );
        // Given no line, the gate is the repository's contract and lock as
        // they are: refused as the repository's own is.
        let as_is = derive(&own, &Overlay::default());
        assert_eq!(as_is.lock, stale.lock);
        // A contract ohjaa cannot read is the repository's, unchanged.
        let odd = derive(
            &Own {
                contract: b"{}\n".to_vec(),
                lock: None,
            },
            &Overlay::standard(),
        );
        assert_eq!(odd.contract, b"{}\n");
        assert!(odd.unread.is_some());
    }

    #[test]
    fn the_overlay_is_read_from_repo_toml() {
        assert_eq!(Overlay::from_table(None).unwrap(), Overlay::standard());
        let empty = std::collections::BTreeMap::new();
        assert!(Overlay::from_table(Some(&empty)).unwrap().is_empty());
        let mut one = std::collections::BTreeMap::new();
        one.insert("CARGO_TARGET_DIR".to_owned(), "x".to_owned());
        assert_eq!(
            Overlay::from_table(Some(&one)).unwrap().lines(),
            [("CARGO_TARGET_DIR".to_owned(), "x".to_owned())]
        );
        one.insert("RUSTFLAGS".to_owned(), "y".to_owned());
        assert!(Overlay::from_table(Some(&one))
            .unwrap_err()
            .contains("unknown key RUSTFLAGS"));
        let text = format!("branch = \"main\"\n{}", Overlay::standard().toml(None));
        let tables = crate::flat_toml::parse(&text).unwrap();
        assert_eq!(
            Overlay::from_table(tables.get("cargo_env")).unwrap(),
            Overlay::standard()
        );
        let none = Overlay::default().toml(Some("no sykli here expands it"));
        assert!(none.contains("# None: no sykli here expands it."), "{none}");
        let tables = crate::flat_toml::parse(&none).unwrap();
        assert!(Overlay::from_table(tables.get("cargo_env"))
            .unwrap()
            .is_empty());
    }

    /// One `git cat-file --batch`, asked one object at a time: a commit,
    /// its files, what is missing, and its parent, as git has them.
    #[test]
    fn a_clone_is_asked_one_object_at_a_time_in_one_process() {
        let dir = std::env::temp_dir().join(format!("ohjaa-objects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(["-c", "user.name=T", "-c", "user.email=t@x", "-C"])
                .arg(&dir)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README"), "x\n").unwrap();
        git(&["add", "README"]);
        git(&["commit", "-q", "-m", "first"]);
        std::fs::write(dir.join("sykli.json"), GATE).unwrap();
        git(&["add", "sykli.json"]);
        git(&["commit", "-q", "-m", "a contract\n\nwith a body"]);
        let mut objects = Objects::of_clone(&dir.join(".git")).unwrap();
        let head = objects.commit("refs/heads/main").unwrap().unwrap();
        assert_eq!(head.oid, git(&["rev-parse", "HEAD"]));
        assert_eq!(head.tree, git(&["rev-parse", "HEAD^{tree}"]));
        assert_eq!(head.subject, "a contract");
        assert_eq!(head.author, "T");
        let Holds::Own(own) = objects.holds_at(&head.oid).unwrap() else {
            panic!("its own sykli.json")
        };
        assert_eq!(own.contract, GATE.as_bytes());
        assert_eq!(own.lock, None);
        assert_eq!(objects.get(&format!("{}:nothing", head.oid)).unwrap(), None);
        let first = objects
            .commit(head.parent.as_deref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(first.parent, None);
        assert_eq!(
            objects.holds_at(&first.oid).unwrap(),
            Holds::Neither {
                tree: first.tree.clone()
            },
            "no sykli.json: its tree"
        );
        assert!(objects.get("bad\nname").is_err());
        assert!(objects.commit(&head.tree).is_err(), "a tree is no commit");
        drop(objects);
        // A sykli.json that is a symbolic link, or a folder, is said so:
        // never read as no contract, or as the file it names.
        std::fs::remove_file(dir.join("sykli.json")).unwrap();
        std::os::unix::fs::symlink("README", dir.join("sykli.json")).unwrap();
        git(&["add", "sykli.json"]);
        git(&["commit", "-q", "-m", "a link"]);
        let linked = git(&["rev-parse", "HEAD"]);
        git(&["rm", "-q", "sykli.json"]);
        std::fs::create_dir_all(dir.join("sykli.json")).unwrap();
        std::fs::write(dir.join("sykli.json/x"), "x\n").unwrap();
        git(&["add", "sykli.json"]);
        git(&["commit", "-q", "-m", "a folder"]);
        let folder = git(&["rev-parse", "HEAD"]);
        // A sykli.rs and no sykli.json: a contract of its own, which no
        // gate is derived from (never what sykli init writes).
        git(&["rm", "-q", "-r", "sykli.json"]);
        std::fs::write(dir.join("sykli.rs"), "fn main() {}\n").unwrap();
        git(&["add", "sykli.rs"]);
        git(&["commit", "-q", "-m", "an emitter"]);
        let emitter = git(&["rev-parse", "HEAD"]);
        let mut objects = Objects::of_clone(&dir.join(".git")).unwrap();
        let said = objects.holds_at(&linked).unwrap_err();
        assert!(said.contains("symbolic link"), "{said}");
        let said = objects.holds_at(&folder).unwrap_err();
        assert!(said.contains("not a file"), "{said}");
        assert_eq!(objects.holds_at(&emitter).unwrap(), Holds::Emitter);
        // The session goes on after a refusal.
        assert!(matches!(
            objects.holds_at(&head.oid).unwrap(),
            Holds::Own(_)
        ));
        drop(objects);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_commit_object_reads_as_git_log_shows_it() {
        let text = "tree 1111\nparent 2222\nparent 3333\n\
                    author Ann Author <ann@example.invalid> 1700000000 +0200\n\
                    committer C <c@x> 1700000060 +0000\n\
                    gpgsig -----BEGIN PGP SIGNATURE-----\n \n abc\n -----END PGP SIGNATURE-----\n\
                    \n\
                    Fix the parser\nacross two lines\n\nThe body.\n";
        let commit = parse_commit("abcd", text.as_bytes()).unwrap();
        assert_eq!(commit.tree, "1111");
        assert_eq!(commit.parent.as_deref(), Some("2222"), "the first parent");
        assert_eq!(commit.author, "Ann Author");
        assert_eq!(commit.authored, 1_700_000_000);
        assert_eq!(commit.subject, "Fix the parser across two lines");
        let root = parse_commit("r", b"tree 1\nauthor A <a> 5 +0000\n\nroot\n").unwrap();
        assert_eq!(root.parent, None);
        assert_eq!(root.subject, "root");
        assert_eq!(
            parse_commit("x", b"author A <a> 5 +0000\n\nno tree\n"),
            None
        );
    }
}
