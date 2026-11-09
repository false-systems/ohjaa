# Route everyday `claude`, `codex`, `pi` and `opencode` through `ohjaa run`
# (workflow step 1 · Start).
#
# Printed by `ohjaa shell-init zsh`; `ohjaa apply` loads it from ~/.zshrc with
#   [[ -x <ohjaa> ]] && eval "$(<ohjaa> shell-init zsh)"
# where <ohjaa> is the path of the ohjaa that ran apply.
#
# ohjaa runs the agent as a child through `toimija run`. A child process never
# sees these shell functions, so the real binary runs there and there is no
# recursion.
#
# Bypasses: `command claude …` for one call, or OHJAA_WRAP=0 for a whole shell.

# Intent for a session when you don't name one: OHJAA_INTENT="fix x" claude
: ${OHJAA_DEFAULT_INTENT:=interactive}

_ohjaa_at_terminal() { [[ -t 0 && -t 1 ]]; }

# Decide whether this invocation should go through ohjaa.
#   $1    the agent: claude | codex | pi | opencode
#   $@:2  the arguments you typed after it
# Return 0 to wrap, 1 to run the agent directly.
_ohjaa_should_wrap() {
  local agent=$1; shift
  # Switched off, or already inside a session: wrapping again would nest it.
  [[ ${OHJAA_WRAP:-1} == 0 || -n $OHJAA_SESSION ]] && return 1
  # No person at a terminal: headless, piped or scripted. No briefing to read,
  # no handover to write.
  _ohjaa_at_terminal || return 1
  # Any folder, a repository or not: only the briefing carries the team's
  # knowledge (ours), which no instruction file holds, so an agent started in
  # the home folder needs it as much as one started in a repository. Outside
  # every repository ohjaa keeps the session as the folder's.
  # One-shots, maintenance and resumed conversations stay direct.
  local arg
  for arg in "$@"; do
    case $agent:$arg in
      *:-h|*:--help|*:--version|*:-V|*:-v) return 1 ;;
      claude:-p|claude:--print|claude:-c|claude:--continue|claude:-r|claude:--resume) return 1 ;;
      codex:resume|codex:fork) return 1 ;;
      # pi 1.4: one-shots, a session resumed, named or forked, and the flags
      # that print or export something and exit.
      pi:-p|pi:--print|pi:-c|pi:--continue|pi:-r|pi:--resume|pi:--session|pi:--session-id|pi:--fork|pi:--export|pi:--list-models) return 1 ;;
      # opencode 2.2: a session continued, and the flags that print and exit.
      opencode:-c|opencode:--continue|opencode:-s|opencode:--session|opencode:--completions|opencode:--wizard) return 1 ;;
    esac
  done
  # pi's --mode json and rpc are for programs, not a person.
  if [[ $agent == pi ]]; then
    local mode=${argv[(Ie)--mode]}
    (( mode )) && [[ ${argv[mode+1]-} != text ]] && return 1
  fi
  case $agent:${1-} in
    claude:mcp|claude:config|claude:doctor|claude:update|claude:install|claude:login|claude:logout|claude:setup-token|claude:migrate-installer|claude:plugin) return 1 ;;
    codex:exec|codex:e|codex:login|codex:logout|codex:mcp|codex:mcp-server|codex:app-server|codex:completion|codex:debug|codex:apply|codex:a|codex:cloud|codex:features|codex:sandbox|codex:help|codex:proto) return 1 ;;
    pi:install|pi:remove|pi:uninstall|pi:update|pi:list|pi:config|pi:auth|pi:mcp) return 1 ;;
  esac
  # opencode's first argument that is not a flag is a subcommand, else the
  # folder to start in. Only the TUI, `mini` and a folder start a session a
  # person types into; `run` and the rest stay direct.
  if [[ $agent == opencode ]]; then
    local first
    first=$(_ohjaa_opencode_first "$@")
    case $first in
      ''|mini) ;;
      upgrade|update|uninstall|acp|api|debug|auth|mcp|plugin|models|stats|run|session|service|reload|pair|serve) return 1 ;;
      *) [[ -d $first ]] || return 1 ;;
    esac
  fi
  return 0
}

# Print opencode's first argument that is neither a flag nor a flag's value.
_ohjaa_opencode_first() {
  while (( $# )); do
    case $1 in
      --server|--prompt|-s|--session|--log-level|--completions|-m|--model|--agent|--replay-limit) shift ;;
      -*) ;;
      *) print -r -- "$1"; return ;;
    esac
    shift
  done
}

# toimija gives the agent its briefing as the final prompt argument (opencode's
# after --prompt), so a prompt you typed must not travel as a second one. Split
# the arguments into the flags that pass through (_ohjaa_pass) and your prompt,
# which becomes the session's task (_ohjaa_task). Flags that take a value are
# listed so their value isn't mistaken for a prompt. Returns 1 when it can't
# tell (two or more prompt-like arguments).
_ohjaa_split() {
  local agent=$1; shift
  local -a value_flags
  local places=0
  case $agent in
    claude) value_flags=(--model --permission-mode --add-dir --agent --agents --settings
                         --setting-sources --mcp-config --session-id --fallback-model
                         --append-system-prompt --system-prompt --allowedTools --allowed-tools
                         --disallowedTools --disallowed-tools --output-format --input-format
                         --plugin-dir --betas) ;;
    codex)  value_flags=(-c --config -m --model -p --profile --local-provider -s --sandbox
                         -C --cd --add-dir --enable --disable -a --ask-for-approval
                         --remote --remote-auth-token-env -i --image) ;;
    pi)     value_flags=(--provider --model --api-key --system-prompt --append-system-prompt
                         --mode --session-dir -n --name --models -t --tools -xt --exclude-tools
                         --thinking -e --extension --skill --prompt-template --theme --use-theme
                         --tui-mode) ;;
    opencode) value_flags=(--server --log-level -m --model --agent --replay-limit) ;;
  esac
  _ohjaa_pass=() _ohjaa_task=()
  while (( $# )); do
    if [[ $agent == opencode && ( $1 == --prompt || $1 == --prompt=* ) ]]; then
      # opencode's prompt is a flag's value, and toimija's --prompt would
      # replace yours: it becomes the task.
      (( ${#_ohjaa_task} )) && return 1
      if [[ $1 == --prompt=* ]]; then
        _ohjaa_task=(--task "${1#--prompt=}")
      elif (( $# > 1 )); then
        _ohjaa_task=(--task "$2"); shift
      fi
    elif [[ $agent == pi && $1 == @* ]]; then
      # A file pi attaches to the first message: it goes with the briefing.
      _ohjaa_pass+=("$1")
    elif [[ $agent == opencode && $1 != -* ]]; then
      # `mini`, else the folder opencode starts in: ohjaa launches from that
      # folder instead, so the briefing and the session are that folder's.
      if (( ++places == 1 )) && [[ $1 == mini ]]; then
        _ohjaa_pass+=("$1")
      elif [[ -z $_ohjaa_dir && -d $1 ]]; then
        _ohjaa_dir=$1
      else
        return 1
      fi
    elif [[ $1 == -* ]]; then
      _ohjaa_pass+=("$1")
      if (( ${value_flags[(Ie)$1]} )) && (( $# > 1 )); then
        _ohjaa_pass+=("$2"); shift
        # Codex's -i/--image takes every value up to the next flag.
        if [[ $agent == codex && ( $_ohjaa_pass[-2] == -i || $_ohjaa_pass[-2] == --image ) ]]; then
          while (( $# > 1 )) && [[ $2 != -* ]]; do _ohjaa_pass+=("$2"); shift; done
        fi
      fi
    elif (( ${#_ohjaa_task} )); then
      return 1
    else
      _ohjaa_task=(--task "$1")
    fi
    shift
  done
}

_ohjaa_launch() {
  local agent=$1; shift
  ohjaa run "$agent" --intent "${OHJAA_INTENT:-$OHJAA_DEFAULT_INTENT}" "${_ohjaa_task[@]}" -- "${_ohjaa_pass[@]}"
}

claude() {
  local -a _ohjaa_pass _ohjaa_task
  if _ohjaa_should_wrap claude "$@"; then
    if _ohjaa_split claude "$@"; then
      _ohjaa_launch claude; return
    fi
    print -u2 "ohjaa: not wrapped (more than one prompt-like argument); put the prompt first, or use: command claude …"
  fi
  command claude "$@"
}

codex() {
  local -a _ohjaa_pass _ohjaa_task
  if _ohjaa_should_wrap codex "$@"; then
    if _ohjaa_split codex "$@"; then
      _ohjaa_launch codex; return
    fi
    print -u2 "ohjaa: not wrapped (more than one prompt-like argument); put the prompt first, or use: command codex …"
  fi
  command codex "$@"
}

pi() {
  local -a _ohjaa_pass _ohjaa_task
  if _ohjaa_should_wrap pi "$@"; then
    if _ohjaa_split pi "$@"; then
      _ohjaa_launch pi; return
    fi
    print -u2 "ohjaa: not wrapped (more than one prompt-like argument); put the prompt first, or use: command pi …"
  fi
  command pi "$@"
}

opencode() {
  local -a _ohjaa_pass _ohjaa_task
  local _ohjaa_dir
  if _ohjaa_should_wrap opencode "$@"; then
    if _ohjaa_split opencode "$@"; then
      if [[ -n $_ohjaa_dir ]]; then
        ( builtin cd -q -- "$_ohjaa_dir" && _ohjaa_launch opencode ); return
      fi
      _ohjaa_launch opencode; return
    fi
    print -u2 "ohjaa: not wrapped (two prompts, or a second folder); use: command opencode …"
  fi
  command opencode "$@"
}
