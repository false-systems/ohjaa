//! `ohjaa doctor`: what differs from what `apply` would make it. Read-only.
//!
//! It reads its facts once: git's global config in one process, then the
//! files apply writes into, and `~/.zshrc`, where an earlier apply wrote a
//! line that apply now takes out.

use anyhow::Result;

use crate::apply::{ignore_with_block, launch_target, read_or_empty, IGNORED, IGNORE_BEGIN};
use crate::gitconfig;
use crate::home::{self, absolute};

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
            found.drift(format!(
                "cannot read the knowledge for agents' own files: {error:#}"
            ));
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

pub fn doctor() -> Result<i32> {
    let home = absolute(home::ohjaa_home()?);
    let mut found = Findings { drift: 0 };
    println!("ohjaa doctor: the workflow home is {}", home.display());

    let global = match gitconfig::GlobalFacts::read() {
        Ok(global) => Some(global),
        Err(error) => {
            found.drift(format!("cannot read git's global config: {error:#}"));
            None
        }
    };
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
    // The terminal's line an earlier apply wrote runs a command this ohjaa
    // does not have: every new terminal would say so.
    match home::zshrc().and_then(|path| read_or_empty(&path).map(|text| (path, text))) {
        Ok((path, text)) => {
            let left: Vec<usize> = text
                .lines()
                .enumerate()
                .filter(|(_, line)| launch_target(line).is_some())
                .map(|(at, _)| at + 1)
                .collect();
            if left.is_empty() {
                found.ok(format!("{} runs nothing of ohjaa's", path.display()));
            }
            for at in left {
                found.drift(format!(
                    "{} line {at} runs `ohjaa shell-init`, which this ohjaa does not have; \
                     every new terminal says so (ohjaa apply takes it out)",
                    path.display()
                ));
            }
        }
        Err(error) => found.drift(format!("cannot read ~/.zshrc: {error:#}")),
    }
    check_instructions(&mut found);
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
