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
the process. Rue installs no signal handlers, so such signals keep their
platform-default behavior. This section describes the observable exit status in
that case; because the trigger is external to the language, these paragraphs are
informative rather than normative.

{{ rule(id="8.5:2", cat="informative") }}

On a Unix host, a process terminated by signal number `signum` reports the exit
status `128 + signum` under the conventional shell encoding (for example, the
`$?` value observed by a parent shell). This follows the platform convention and
is not defined by Rue itself; the exact encoding is that of the host operating
system and its shell.

{{ rule(id="8.5:3", cat="informative") }}

A Rue program whose standard output (or any output stream it writes to) is a
pipe or socket **whose reading end has been closed** is terminated by `SIGPIPE`
on its next write to that stream, exiting with status `141` (that is,
`128 + SIGPIPE`, where `SIGPIPE` is `13` on Linux and macOS). This is Rue's
defined default: it matches the standard Unix behavior for programs in a
pipeline (such as `rue_program | head`), so a downstream reader that stops
consuming input promptly and cleanly ends the producer. The runtime's write path
does **not** intercept this case — the signal is delivered before the failing
`write` syscall returns, so the write's error result is never observed (see the
`write_stderr`/`write_stdout` documentation in `rue-runtime`).

{{ rule(id="8.5:4", cat="informative") }}

A write that fails *without* a signal — for example, writing to a file
descriptor that has been closed outright (`EBADF`) rather than to a broken pipe
— does not terminate the program. The runtime discards the error and execution
continues, because there is no meaningful recovery and the process is typically
about to exit regardless.

{{ rule(id="8.5:5") }}

Making the `SIGPIPE` response configurable (an analogue of Rust's
`-Zon-broken-pipe`, e.g. resetting the disposition so writes observe `EPIPE`
instead of dying) is a separate, future feature and is intentionally out of
scope here; the default described above is the only behavior Rue currently
provides.
