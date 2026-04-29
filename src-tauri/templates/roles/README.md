# Role templates — author guide

Each `.toml` file in this directory defines a fast-path role for the
**Commissioning** flow. Templates are embedded into the OrionII binary at
compile time via `include_str!` in `src-tauri/src/orion/charter_template.rs`,
so adding or editing one requires a Rust rebuild.

## File layout

```toml
key = "calendar_assistant"           # stable identifier; matches role_key on the wire
display_name = "Calendar Assistant"  # cockpit card heading
description = "..."                  # one sentence, <120 chars
time_estimate_minutes = 3            # sets expectations on the role-pick grid
charter_template = """               # MUST come before the [[slots]] tables
# Calendar Assistant — {{ organization }}
...
"""

[[slots]]
key = "organization"                 # placeholder name in the template
label = "Organization name"          # shown above the input in the cockpit
kind = "text"                        # text | url | timezone | multiline
```

## Why `charter_template` must be first

TOML scoping: scalar assignments after a `[[slots]]` array-of-tables
header bind to that array's last element, not to the document root. If
you put `charter_template` at the bottom it parses as a slot field and
silently disappears. Always put scalars (key, display_name, ...,
charter_template) before any `[[slots]]`.

## Slot rules

- Every slot is required at v1. Optional slots are not yet supported;
  the renderer rejects an empty value with `RoleError::MissingSlot`.
- Slot `key` must match its placeholder in `charter_template`. The
  renderer accepts `{{ key }}`, `{{key}}`, and `{{  key  }}`
  (single-space, no-space, double-space).
- `kind` controls the input widget on the cockpit:
  - `text` → single-line `<input>`
  - `url` → single-line `<input>` (no extra validation in v1)
  - `timezone` → single-line `<input>` (no picker in v1)
  - `multiline` → `<textarea>`

## Voice

Charter copy is the voice the operator sees on every audit and review.
Keep it business-direct:

- Lead with **purpose** in one short paragraph.
- State **scope** (which systems, which data) explicitly.
- End with **boundaries** — what the agent must never do without
  operator confirmation. Be concrete; vague boundaries undercut audit.

Avoid mystical or first-person framing ("I will live..."). The charter
is read by compliance and procurement, not just the operator.

## Adding a new role

1. Copy an existing TOML file as a starting point.
2. Bump the count in the
   `every_template_loads_and_renders_without_unfilled_placeholders` test
   in `charter_template.rs` so the test asserts your new template loads.
3. `cargo test --lib` to verify rendering covers every slot.
4. Rebuild the MSI (`npm run build:installer`); the new role surfaces
   on the cockpit's role-pick grid automatically.

## Keeping templates short

The fast path is meant to take ~3-5 minutes. Six slots is a soft ceiling.
If a role wants more configuration, push the extra detail into the Q&A
path or a charter amendment after commissioning — do not bloat the
fast-path form.
