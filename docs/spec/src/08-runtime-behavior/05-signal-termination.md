+++
title = "Signal Termination"
weight = 5
template = "spec/page.html"
+++

# Signal Termination

{{ rule(id="8.5:1", cat="informative") }}

The traps of the preceding sections (overflow, division by zero, out-of-bounds
indexing) end a program *from inside* with the panic exit code 101. A program
may also be ended *from outside* the language's control flow when the host
operating system delivers a **signal** whose default disposition is to terminate
the process. Rue installs one signal handler and no others: `SIGSEGV` is caught
so that a stack overflow becomes a clean abort (8.5:6), and every other signal
keeps its platform-default behavior. This section describes the observable exit
status in that case; because the trigger is external to the language, these
paragraphs are informative rather than normative.

{{ rule(id="8.5:6", cat="informative") }}

The `SIGSEGV` carve-out exists because exhausting the stack is not really an
external event: unbounded recursion, or a frame larger than the space that
remains, faults on the operating system's guard page, and the default
disposition would kill the process with the raw crash status `139`
(`128 + SIGSEGV`, per 8.5:2) and no explanation. Before user code runs, the
runtime instead
installs a `SIGSEGV` handler on an alternate signal stack — so the handler can
run even though the main stack is the thing that overflowed — which writes
`stack overflow` to standard error and exits with the panic exit code `101`,
the same code the traps of the preceding sections use. Any `SIGSEGV` is reported
this way: in safe Rue a blown stack is the only way to raise one, because
indexing is bounds-checked, there are no raw-pointer dereferences, and
arithmetic traps rather than corrupting memory. A `SIGSEGV` raised from
`checked` code (chapter 9), where those guarantees do not hold, is undefined
behavior under B.3 and is reported the same way regardless of its cause.
Installing the handler is best-effort: on a host where the alternate stack or
the handler registration is unavailable, the program keeps the default
`SIGSEGV` disposition and exits with `139` as described above.

{{ rule(id="8.5:2", cat="informative") }}

On a Unix host, a process terminated by signal number `signum` reports the exit
status `128 + signum` under the conventional shell encoding (for example, the
`$?` value observed by a parent shell). This follows the platform convention and
is not defined by Rue itself; the exact encoding is that of the host operating
system and its shell.

{{ rule(id="8.5:3", cat="informative") }}

A Rue program whose standard output (or another stream written without
per-operation signal suppression) is a pipe or socket **whose reading end has
been closed** is terminated by `SIGPIPE` on its next failing write to that
stream, exiting with status `141` (that is, `128 + SIGPIPE`, where `SIGPIPE` is
`13` on Linux and macOS). This is Rue's defined default: it matches the standard
Unix behavior for programs in a pipeline (such as `rue_program | head`), so a
downstream reader that stops consuming input promptly and cleanly ends the
producer. The runtime's standard-output and standard-error write paths do
**not** intercept this case — the signal is delivered before the failing
`write` syscall returns, so the write's error result is never observed (see the
`write_stderr`/`write_stdout` documentation in `rue-runtime`).

Linux `std.net.TcpStream.write` and `write_all` are a socket-scoped exception:
each send uses `MSG_NOSIGNAL`, so a closed peer returns
`NetworkError.ConnectionReset` (from `EPIPE` or `ECONNRESET`) instead of
terminating the process. This per-send flag does not change the process signal
disposition and therefore does not affect standard output, standard error, or
raw `@syscall` writes.

{{ rule(id="8.5:4", cat="informative") }}

A write that fails *without* a signal — for example, writing to a file
descriptor that has been closed outright (`EBADF`) rather than to a broken pipe
— does not terminate the program. The runtime discards the error and execution
continues, because there is no meaningful recovery and the process is typically
about to exit regardless.

{{ rule(id="8.5:5") }}

Making the process-wide `SIGPIPE` response configurable (an analogue of Rust's
`-Zon-broken-pipe`, e.g. resetting the disposition so ordinary writes observe
`EPIPE` instead of dying) is a separate, future feature and is intentionally out
of scope here. The default disposition described above remains the only
process-wide behavior Rue provides; `std.net`'s per-send socket flag does not
configure or alter it.
