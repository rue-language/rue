# Local MCP server

Rue provides a local stdio Model Context Protocol server for coding agents. It
is a thin adapter over the compiler's existing machine-readable interfaces: it
runs the real filesystem driver with `--error-format json`, reads explanations
and error metadata from `rue-error`, and runs the schema-v1 specification index
producer. It does not host a daemon or add another compiler frontend.

Run it through Buck so the canonical producer binaries and specification inputs
are supplied explicitly:

```console
./buck2 run //crates/rue-mcp:server
```

Configure an MCP client with that command, the repository root as its working
directory, and stdio transport. The server implements MCP revision
`2026-07-28`; clients may start with `server/discover` and then use
`tools/list` and `tools/call`. Every request must carry that version and client
capabilities in `params._meta` as required by that revision.

The tools are:

- `compile`: compile exactly one root module to a new explicit output path.
- `check`: run the same complete compiler path and remove its temporary
  executable, retaining only the result and canonical diagnostics.
- `explain-error`: query a compiler-owned structured E-code explanation.
- `error-metadata`: query the complete compiler-owned error inventory.
- `spec`: query the canonical schema-v1 specification index, with
  optional exact `specId` and `errorCode` filters.

Compilation tools accept an optional `sourceManifest`. Agents operating in a
bounded build environment should always pass it. Module discovery remains the
driver's root-transitive `@import` traversal: additional positional sources are
not accepted, and the server has no peer project-loading policy. Both tools pin
the canonical internal linker. They compile into an atomically created private
directory owned by the server; only a successful `compile` publishes the
executable, by staging it in the destination directory and using an atomic
no-clobber persist. Copying, permissions, and file synchronization happen before the
cancellation/response claim; only the atomic no-clobber publication is inside
that boundary. Existing destinations are rejected. This keeps compiler
`PendingOutput` files and interrupted output away from caller paths, and the
private directory is removed on every result path.

On Unix, each producer runs in its own process group, so cancellation and EOF
terminate the producer and any descendants (including platform signing tools)
before the request worker is reaped. On non-Unix hosts, Rust's portable process
API can guarantee termination only of the direct producer; Rue MCP does not
claim descendant-tree cleanup there.

Tool results contain both `structuredContent` and an equivalent serialized JSON
text content block for client compatibility. A rejected Rue program is a normal
tool result with `success: false` and the diagnostic objects from
[`--error-format json`](diagnostics.md) preserved. Tool-origin failures use
`isError: true`; protocol errors use JSON-RPC errors. Closing stdin terminates
in-flight compiler children. `notifications/cancelled` terminates the child for
its `requestId` and, as required by MCP 2026-07-28, suppresses its response.
The server accepts at most eight in-flight requests, one MiB per incoming
newline-delimited message, and eight MiB for each producer output stream.
Producer output is drained concurrently from pipes into fixed-cap buffers. On
Unix, descendant teardown closes inherited writers before the readers finish;
on other hosts, reader supervision times out rather than hanging if a
descendant retains a writer. Requests over those bounds fail deterministically
instead of accumulating unbounded memory or process handles.
