//! `ohjaa shell-init zsh`: the human view's line for a terminal's opening,
//! printed for `~/.zshrc` to eval. Agents start as they are typed: the rules
//! reach each through its own instruction file (`ohjaa apply`).

use anyhow::Result;

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
    print!("{LINE_ZSH}");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_runs_once_as_the_shell_starts_and_can_be_switched_off() {
        let lines: Vec<&str> = LINE_ZSH.lines().filter(|l| !l.starts_with('#')).collect();
        assert!(lines.contains(&"[[ ${OHJAA_LINE:-1} == 0 ]] || ohjaa line"));
        // Never inside a function: it runs at the eval, once.
        assert!(!LINE_ZSH.contains("() {"));
    }

    #[test]
    fn no_agent_is_wrapped() {
        for agent in ["claude", "codex", "pi", "opencode"] {
            assert!(!LINE_ZSH.contains(&format!("{agent}()")), "{agent}");
        }
        assert!(!LINE_ZSH.contains("ohjaa run"));
    }
}
