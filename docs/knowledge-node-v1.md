# knowledge-node.v1

The file format for one unit of [Knowledge](ontology.md#knowledge).

- The reference implementation is `crates/knowledge-node`.
- The golden fixtures are in `fixtures/knowledge-node.v1/`.
- An implementation conforms when it gives every fixture the expected result.

**Frozen.** A test pins the fixtures. Changing what a valid node is means
releasing `knowledge-node.v2`, not editing v1.

## One node, one file

A node is a UTF-8 file named `<id>.md`:

```
+++
id = "lima-for-linux"
kind = "fact"
status = "current"
as_of = 2026-09-24
origin = ["15 corrections, 2026-05-28 to 2026-09-05: 'we have lima'"]
holds_while = ["command:limactl"]
+++
Linux and eBPF builds and tests run in the Lima VM. Never report them as
blocked on macOS.
```

- **Frontmatter.** TOML between two lines that are exactly `+++`. The first line
  of the file must be `+++`. A trailing `\r` on a delimiter line is ignored. The
  frontmatter ends at the first `+++` line, even one inside a multi-line TOML
  string, so the canonical form writes every value on one line.
- **Body.** Everything after the closing `+++`, with leading and trailing
  whitespace removed. It is what an Agent reads, and it must not be empty.

TOML rather than YAML: TOML has no implicit typing, so `no` is never `false` and
a date is never a string.

## Fields

| Field | Required | Type | Meaning |
|---|---|---|---|
| `id` | yes | string | `[a-z0-9]+(-[a-z0-9]+)*`, at most 64 characters, equal to the file stem |
| `kind` | yes | string | `fact`, `decision`, `taste` or `lesson` |
| `status` | yes | string | `proposed`, `current`, `superseded` or `retired` |
| `as_of` | yes | TOML local date | The date this Knowledge was last confirmed. Unquoted, with no time or offset. |
| `origin` | yes | array of strings | Why it exists: the correction, decision or Outcome behind it. Must not be empty, and no entry may be blank. |
| `supersedes` | no | array of ids | The nodes this one replaces. No duplicates, and never its own id. |
| `refines` | no | id | The node this one narrows or builds on. Never its own id. |
| `holds_while` | no | array of predicates | Facts that must stay true for this node to hold |

Any other field is an error.

**Scope is not a field.** A node's scope (`ours`, `repo`, `mine`, `machine`) is
the directory it lives in. A field could disagree with its location; a
location cannot.

## Predicates

| Predicate | Holds when |
|---|---|
| `command:<name>` | an executable named `<name>` is on `PATH`. `<name>` uses only `A-Z a-z 0-9 . _ + -`. |
| `path:<path>` | the path exists. It is absolute (`/…`), home-relative (`~/…`), or relative to the project root, with no `..` component. |

An implementation evaluates predicates; the format only defines them. A
predicate that fails flags the node for review. It never deletes the node or
hides it silently.

## Rules across a set of nodes

A set is every node loaded together from the four scopes.

1. An id appears once across the whole set.
2. Every `supersedes` and `refines` target exists in the set.
3. A node supersedes only nodes **in its own scope**. Replacement never crosses
   ownership.
4. A node refines only nodes **at least as shared** as itself, in the order
   `ours` → `repo` → `mine` → `machine`. Shared Knowledge never builds on
   personal Knowledge.
5. When a `current` or `superseded` node supersedes a target, the target must be
   `superseded`. A `proposed` node may name a target that is still `current`;
   accepting the proposal is what supersedes it.
6. A `superseded` node is named in the `supersedes` list of some node. A
   supersession always has a successor.
7. There are no cycles through `supersedes` or `refines`.

## Briefing order

The briefing is what an Agent reads before acting. It contains exactly the
`current` nodes, ordered by:

1. scope: `ours`, `repo`, `mine`, `machine`;
2. then kind: `decision`, `fact`, `taste`, `lesson`;
3. then id.

How the briefing is rendered belongs to the tool. Which nodes it contains, and
in what order, belongs to the contract.

## Canonical form

The reference implementation writes a node back out in one canonical form:

- fields in the order of the table above;
- empty optional fields omitted;
- the body followed by one newline.

Parsing the canonical form gives back the same node. Tools that edit nodes
write this form, so every change shows up as a minimal diff.

## Error codes

These are stable strings. The fixtures name them.

| Code | Meaning |
|---|---|
| `missing-frontmatter` | The file does not start with `+++`, or the frontmatter is never closed |
| `bad-toml` | The frontmatter is not valid TOML |
| `unknown-field` | A field this format does not define |
| `missing-field` | A required field is absent |
| `wrong-type` | A field has the wrong TOML type |
| `bad-id` | An id, or a referenced id, breaks the id syntax |
| `id-mismatch` | The id is not the file stem |
| `bad-kind` | `kind` is not one of the four kinds |
| `bad-status` | `status` is not one of the four statuses |
| `bad-date` | `as_of` is not an unquoted, date-only TOML date. A date that doesn't exist, such as `2026-02-30`, is already invalid TOML and is reported as `bad-toml`. |
| `empty-origin` | `origin` is empty, or has a blank entry |
| `empty-body` | Nothing after the frontmatter |
| `bad-predicate` | A `holds_while` entry breaks the predicate syntax |
| `self-reference` | A node supersedes or refines itself |
| `duplicate-reference` | `supersedes` names the same id twice |
| `duplicate-id` | Set rule 1 |
| `dangling-reference` | Set rule 2 |
| `cross-scope-supersedes` | Set rule 3 |
| `refines-narrower-scope` | Set rule 4 |
| `supersedes-live-node` | Set rule 5 |
| `unexplained-supersession` | Set rule 6 |
| `cycle` | Set rule 7 |

When a node has several faults, it is reported under the first one found. The
checks run in this order:

1. frontmatter
2. TOML
3. unknown fields
4. missing fields
5. then each field in table order, followed by the body
