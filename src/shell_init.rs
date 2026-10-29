//! `ohjaa shell-init zsh`: the launcher that routes `claude` and `codex`
//! through `ohjaa run`, printed for `~/.zshrc` to eval, and the human view's
//! line for a terminal's opening.

use anyhow::Result;

/// The launcher, printed by `ohjaa shell-init zsh`. Its rules are the
/// person's own: see the comments in the file.
const LAUNCH_ZSH: &str = include_str!("launch.zsh");

/// Run once, as the eval runs: a terminal opening. `ohjaa line` says nothing
/// in an agent's shell or without a terminal, so it needs no guard here.
const LINE_ZSH: &str = "
# Only what needs attention or a decision. Activity stays in `ohjaa digest`.
# Silent in an agent's shell. Off for a shell: OHJAA_LINE=0.
[[ ${OHJAA_LINE:-1} == 0 ]] || ohjaa line
";

pub fn shell_init(shell: &str) -> Result<i32> {
    if shell != "zsh" {
        eprintln!("ohjaa shell-init: only zsh is supported, not {shell}");
        return Ok(2);
    }
    print!("{LAUNCH_ZSH}{LINE_ZSH}");
    Ok(0)
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
    fn the_line_runs_once_as_the_shell_starts_and_can_be_switched_off() {
        let lines: Vec<&str> = LINE_ZSH.lines().filter(|l| !l.starts_with('#')).collect();
        assert!(lines.contains(&"[[ ${OHJAA_LINE:-1} == 0 ]] || ohjaa line"));
        // Never inside a function: it runs at the eval, once.
        assert!(!LINE_ZSH.contains("() {"));
    }
}
