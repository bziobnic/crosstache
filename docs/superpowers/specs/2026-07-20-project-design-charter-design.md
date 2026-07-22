# Project Design Charter

## Purpose

Create a short, binding project-wide charter at `docs/design-principles.md`.
The charter will govern product behavior, public interfaces, architecture,
storage, security, platform support, and release decisions.

The charter is a decision standard, not a statement of preference. Material
feature designs and reviews must demonstrate compliance or record an explicit
exception. Small maintenance changes do not require a ceremonial compliance
check when they cannot affect the charter's concerns.

The canonical charter will be linked from the README and repository
agent/contributor guidance. Existing detailed contracts, such as
`docs/exit-codes.md`, remain authoritative within their narrower scopes.

For this charter, **domain functionality** means operations and queries that
inspect, resolve, transform, or mutate Crosstache-managed secrets, records,
files, vaults, workspaces, configuration, backend state, or security-relevant
context. Presentation-only behavior such as window sizing, native menus, and
desktop notifications does not require a CLI equivalent.

## Authority and exceptions

New features and material changes must comply with the charter. Feature
specifications and reviews must identify the relevant principles and explain
how the design satisfies them.

A departure is permitted only when it records:

- the principle being departed from;
- the rationale;
- the exact scope and user impact;
- whether the exception is temporary or permanent;
- for a temporary exception, its exit condition.

Experimental prototypes may temporarily omit CLI parity only when they are
clearly marked experimental and excluded from normal releases. They are not
complete or production-ready until CLI parity exists. Prototypes remain bound
by the charter's security and data-protection requirements.

When principles conflict, security and protection against irreversible data
loss take precedence. Other conflicts must be resolved explicitly rather than
silently choosing whichever principle is easiest to satisfy.

## Binding design goals

### 1. The CLI is primary and permanent

Every domain operation and query available through the TUI, web UI, desktop
app, or another interface must be possible through the CLI. Equivalent outcomes
and safety guarantees are required; identical interaction sequences are not. A
GUI may compose several CLI-equivalent operations into one workflow.

### 2. Humans and programs are equal CLI users

Interactive use must be designed to be discoverable, concise, and pleasant. Every
interactive workflow must also have a complete non-interactive path through
arguments, standard input, configuration, or environment variables. Automation
must never depend on answering prompts.

### 3. CLI contracts are composable and predictable

Commands provide machine-readable output where their results may be consumed
programmatically. Stdout contains the requested result or a documented
structured envelope; diagnostics and progress use stderr. Exit codes, error
identifiers, schemas, and non-interactive behavior are public contracts.

### 4. Security is the default, not an optional mode

Secret exposure requires explicit intent. Workflows must not require secrets in
command-line arguments, logs, terminal decoration, or unencrypted persistent
storage. Secure prompts and standard input must be available where secrets
enter the system. Destructive actions fail safely and make partial completion
visible.

### 5. All interfaces share one behavioral core

Domain rules, backend selection, configuration resolution, validation, and
security policy live below presentation and transport layers. Frontends adapt
shared operations; they do not independently recreate Crosstache behavior.

### 6. Backend abstraction is honest

Common capabilities have consistent user-facing semantics across backends.
Unsupported operations fail explicitly and explain the limitation. Crosstache
must not conceal meaningful provider differences or reduce every backend to
the lowest common denominator; provider-specific capabilities may be exposed
deliberately.

### 7. Effective context is explainable

Backend, vault, environment, credentials, configuration sources, and precedence
must be inspectable. Crosstache must not silently select a materially different
target or credential path when the requested one is unavailable.

### 8. Core operation is cross-platform and self-contained

The CLI's domain functionality must behave consistently across supported
Linux, macOS, and Windows environments. Crosstache must not require a
Crosstache-hosted account, control plane, persistent daemon, or network service
beyond the user's selected backend.

### 9. Compatibility changes are intentional

Commands, configuration precedence, persisted formats, output schemas, exit
codes, and error identifiers are compatibility surfaces. Breaking changes
require explicit justification, documentation, and a practical migration path.

### 10. Failures are actionable and safe

Errors preserve a stable machine-readable identity, explain the failed
operation without leaking sensitive material, and provide useful human
remediation when known. Partial success, ambiguity, and degraded behavior must
be reported rather than silently accepted.

## Non-goals

1. **Identical interfaces.** CLI, TUI, web, and desktop interfaces do not need
   matching layouts, terminology at every presentation point, or identical
   numbers of steps. They need equivalent access to domain capabilities.
2. **Every interface on every platform.** The cross-platform commitment applies
   to core CLI functionality. Optional interfaces may have narrower platform
   availability when documented.
3. **Perfect backend feature parity.** Crosstache does not promise that Azure,
   AWS, local storage, or future backends support every operation. Capability
   differences are expected and must be represented honestly.
4. **General cloud administration.** Crosstache is not intended to replace
   Azure, AWS, operating-system, or identity-provider administration tools. It
   should perform the administration necessary for its workflows without
   becoming a universal IAM or infrastructure console.
5. **A general-purpose remote secrets service.** The embedded web UI and
   desktop shell are local interfaces, not remotely exposed multi-user servers.
   Core use does not depend on Crosstache operating a hosted control plane.
6. **Permanent stability of human presentation.** Tables, wording, colors,
   progress displays, and other human-oriented presentation may improve over
   time. Machine-facing contracts receive the stronger compatibility guarantee.
7. **Frontends shelling out to the CLI.** CLI primacy does not mean every
   interface must execute the `xv` binary. Interfaces should share the same
   application services and semantics beneath their respective adapters.
8. **Compatibility at any cost.** Existing behavior may change when necessary
   for security, correctness, or coherent design. Such changes must be
   deliberate and include an appropriate migration path; the charter does not
   require preserving every historical mistake forever.

## Anti-goals

1. **GUI-only domain functionality.** A production interface must not become
   the sole way to inspect or change Crosstache-managed state.
2. **Prompt-only workflows.** No operation may require an interactive picker,
   confirmation, editor, or prompt without a complete automation path.
3. **Prose as an API.** Scripts must not be forced to scrape tables, colors,
   progress indicators, or human-readable error messages.
4. **TTY-dependent semantics.** Terminal detection may affect presentation and
   safe interactive conveniences, but must not silently change the target,
   scope, or domain meaning of an operation.
5. **Secret disclosure for convenience.** Secret values must not be exposed to
   output, logs, process listings, telemetry, persistent clipboard state, or
   unencrypted storage by default.
6. **Frontend-specific business logic.** Validation, configuration precedence,
   backend behavior, and security rules must not drift through separate
   implementations in CLI, web, TUI, and desktop code.
7. **Silent fallback or partial success.** Crosstache must not quietly switch
   backend, vault, environment, credential source, or operation mode, nor
   present a partially completed mutation as full success.
8. **Lowest-common-denominator abstraction.** Cross-backend consistency must
   not prevent useful provider-specific capabilities or obscure limitations
   that matter to users.
9. **Infrastructure for frontend convenience.** A mandatory daemon, hosted
   account, control plane, or externally reachable server must not be introduced
   merely to simplify a GUI or transport layer.
10. **Platform surprises.** Supported platforms must not expose commands with
    the same syntax but materially different undocumented effects.
11. **Illusory controls.** Flags, configuration values, and UI controls must
    not be accepted and then ignored. Unsupported combinations should fail
    clearly.
12. **Unbounded exceptions.** Temporary departures must not become permanent
    through neglect, and permanent departures must not remain implicit.

## Feature acceptance checklist

Every material feature or behavioral change must answer:

- What domain operations or queries does this add or change?
- Where is the CLI surface, and can it reach every resulting state?
- Is every interactive workflow also complete in non-interactive use?
- What structured output, exit codes, and stable error identifiers apply?
- Does the implementation reuse shared domain behavior rather than duplicate it
  in a frontend?
- How are secret exposure, destructive actions, and partial failure handled?
- Are backend capability differences explicit?
- Can users inspect the effective backend, vault, environment, credentials, and
  configuration involved?
- What platforms are supported, and are differences documented?
- Does this alter a compatibility surface or persisted format? If so, what is
  the migration path?
- Does it depart from any charter principle? If a question is not applicable,
  why?

## Prototype policy

Experimental prototypes may omit CLI parity only when:

- they are clearly labeled experimental;
- they are excluded from normal production releases;
- users cannot reasonably mistake them for supported functionality;
- the omitted CLI surface and exit condition are recorded;
- they still satisfy security and data-protection requirements.

## Exception policy

Every exception record must identify the principle, rationale, exact scope,
user impact, and whether it is temporary or permanent. Temporary exceptions
require a concrete exit condition. Permanent exceptions must explain why the
project remains coherent despite the departure.

Exceptions cannot silently waive protection against default secret disclosure
or unacknowledged irreversible data loss.

## Documentation placement

Implementation will:

1. Publish the approved charter as `docs/design-principles.md`.
2. Add a prominent link from `README.md`.
3. Add a link from repository agent/contributor guidance so future design and
   implementation work uses the charter as an acceptance standard.
4. Link relevant detailed contracts from the charter instead of duplicating
   their contents.
