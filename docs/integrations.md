# Optional integrations

[Chvrn](../README.md) works as a standalone file comparison, merge and local Git-review tool. Language servers, Herdr and the local socket add capabilities without replacing explicit review and submission.

## Language server

Supply the executable yourself. Chvrn does not discover or start a server without `--lsp`.

For a Rust project with `rust-analyzer` installed on `PATH`:

```sh
chvrn diff /path/to/before.rs /path/to/after.rs --lsp rust-analyzer
```

Pass server arguments with repeated `--lsp-arg` options. `--lsp-arg=--log=error`, for example, passes `--log=error` as one argument to a server that supports it. The options also work with interactive merge and repository review.

| Key | Action on the focused document |
| --- | --- |
| `K` | Request hover information. |
| `D` | Show diagnostics for the current document version. |
| `g` | Open the first returned definition in a temporary read-only view. |
| `F` | Format an editable buffer. |
| Escape | Return from the temporary definition view. |

Formatting is an undoable in-memory edit. It does not write a file; normal `s` submission and merge confirmation still apply. Same-document definitions use inspected in-memory text, including unsaved changes. Cross-file definitions read the target local file. Unsupported capabilities and server errors are visible rather than reported as successful empty results.

### Boundaries and troubleshooting

- The configured server is used for the focused path; this is not automatic per-language server discovery. Switching paths can replace the server process.
- The CLI sends explicit language IDs for Rust, TypeScript/TSX, JavaScript/JSX, Python and JSON. Other extensions, **including C++**, are sent as `plaintext`. A server may infer language from a URI, but do not assume every server will.
- Previous runtime checks exercised real clangd formatting, hover, diagnostics and definitions. That is not a claim of correct C++ language-ID mapping or verification of other servers. The Rust example above describes the supported configuration, not a recorded rust-analyzer compatibility test.
- Requests use LSP JSON-RPC framing. Reads have a 30-second timeout. Only advertised hover, definition and formatting capabilities are available.
- Diagnostics must include a matching document version. Repeated `D` requests reuse only the current version's batch; missing, unversioned or stale batches are not treated as current results.
- Asynchronous responses must still match the focused path, pane and exact buffer identity. Changing or undoing text invalidates old responses, even when the bytes later happen to match.
- Formatting is unavailable in read-only source/base panes. A missing, non-UTF-8 or non-local definition target cannot be opened as an editable document.

The [integration contract](contracts/integrations.md) documents UTF-16 conversion, protocol bounds, buffer identity and library APIs.

## Herdr

Use Herdr integration from a real Herdr session with `HERDR_ENV=1`. Explicit `--herdr` modes fail outside that environment. Chvrn does not install Herdr as part of ordinary installation.

Choose an existing agent name or pane ID. Replace `my-agent` below with that target:

```sh
chvrn review --base HEAD --herdr companion --agent my-agent
chvrn review --base HEAD --herdr auto --agent my-agent
chvrn review --base HEAD --herdr gate --agent my-agent
chvrn review --base HEAD --herdr companion --agent my-agent --open-companion
```

| Mode | Behaviour |
| --- | --- |
| `companion` | Suppress automatic gate offers. Explicit submission and feedback remain available. |
| `auto` | Offer review after a verified transition from working to blocked/done/idle, not merely on initial idle status. |
| `gate` | Open the current review explicitly. Requires interactive terminal input/output. |

`--open-companion` creates a right-hand split without taking focus. Automatic/gate review offers can focus the review pane on a qualifying transition. A stdin patch cannot be forwarded into a new companion split.

Inside Herdr, interactive Git review defaults to `auto` if `--herdr` is omitted. `--agent` takes precedence; otherwise the target is inherited from `HERDR_PANE_ID`. If neither is available, target selection fails. Standalone file diff and merge do not construct this repository-review integration.

To keep a repository review standalone while running inside a Herdr terminal, remove the detection variable for that invocation only:

```sh
env -u HERDR_ENV chvrn review --base HEAD
```

### Lifecycle setup

For omp, install Herdr's official integration:

```sh
herdr integration install omp
herdr agent get my-agent
herdr agent explain my-agent --json
```

Start a **new agent process after installation**, then use the inspection commands on that agent. The existing documented installation was exercised with Herdr 0.9.0. A restart command was not established as a substitute for activating the integration in a new process.

Automatic transitions and feedback require authoritative lifecycle and session identity. Screen text, an unverified idle label or a terminal-output revision is not sufficient. The same terminal can survive an agent/model restart. Chvrn conservatively withholds automatic effects when those identities or verification cannot be established.

### Review and feedback

In an integrated repository review, `E` asks the selected, input-ready agent to explain the selected hunk. It does not automatically edit or accept a change.

Explicit submission records reviewed files, accepted/rejected ranges and comments against the inspected state. When requested, the local report is persisted before feedback is queued. The complete inspected Git state is revalidated before delivery.

An agent's permission/question prompt is **not code approval**. Feedback waits until the verified target can accept input. Chvrn never answers that prompt, sends an approval key or treats quitting as approval. Resolve the agent prompt separately while feedback is pending.

If the transport cannot establish whether delivery succeeded, the UI reports uncertainty rather than retrying blindly and potentially duplicating feedback. A changed agent session, terminal identity or inspected content invalidates pending approval. The [contract](contracts/integrations.md) gives the lifecycle and delivery state machine.

## Private local socket

The socket is an optional interface for tools to propose patches to an **interactive repository review**. It is not an unattended patch-application service or a network API.

```sh
chvrn review --base HEAD --socket /tmp/chvrn-private/review.sock
```

The parent directory must be owner-private. A missing parent is created with mode `0700`; the socket is bound with mode `0600`. An existing shared/wrong-owner parent is refused. Unix filesystem access and inspected content checks form the boundary; there is no separate per-client authentication mechanism.

The listener is not started in headless review. Merely passing `--socket` with redirected input/output does not start a daemon.

### Client flow

1. Connect and send `{"type":"inspect"}` to obtain current per-file paths and snapshot IDs. Do not hard-code example IDs.
2. Send a `patch_candidate` for an inspected path and its current ID. The candidate must match inspected and current disk content and stay within the repository, without symlink traversal.
3. The reviewer presses `v` to preview a queued candidate, then explicitly submits it or presses `x` to decline. Queuing a candidate does not validate every patch operation or authorise a write.
4. Query `review_status` for the decision. Socket proposals use the same Git preview/application checks as other patches.

Each connection carries one request/response. Framing is a four-byte unsigned big-endian byte length followed by UTF-8 JSON, with a maximum body of **65,536 bytes**. Read/write timeouts are three seconds. Full request/response schemas are in the socket section of the [integration contract](contracts/integrations.md).

Accepted/declined receipts are immutable and survive snapshot refresh for the running process, not process restart. Obsolete pending candidates are invalidated. Patch application precedes acknowledgement of acceptance; a receipt failure can therefore occur after files were written and is reported rather than rolled back. Non-UTF-8 native paths remain local-only instead of being silently re-encoded for the protocol.

For ordinary Git review, patch files and write safety, return to the [user guide](usage.md).
