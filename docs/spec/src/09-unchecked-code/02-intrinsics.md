+++
title = "Unchecked Intrinsics"
weight = 2
template = "spec/page.html"
+++

# Unchecked Intrinsics

This section describes intrinsics that require a checked block.

## Syscall Intrinsic

{{ rule(id="9.2:1", cat="normative") }}

The `@syscall` intrinsic performs a direct system call to the operating system.

{{ rule(id="9.2:2", cat="legality-rule") }}

The `@syscall` intrinsic requires the `unchecked_code` preview feature. Using `@syscall` without enabling this feature is a compile-time error.

{{ rule(id="9.2:3", cat="syntax") }}

```ebnf
syscall_intrinsic = "@syscall" "(" syscall_number { "," argument } ")" ;
syscall_number = expression ;
argument = expression ;
```

{{ rule(id="9.2:4", cat="legality-rule") }}

The `@syscall` intrinsic takes at least one argument (the syscall number) and at most seven arguments (syscall number plus six syscall arguments). All arguments must be of type `u64`.

{{ rule(id="9.2:5", cat="dynamic-semantics") }}

The `@syscall` intrinsic returns an `i64` value representing the result of the syscall. On Linux x86-64, negative values typically indicate errors. The exact behavior depends on the syscall being invoked and the platform.

{{ rule(id="9.2:6", cat="informative") }}

Syscall numbers and conventions differ between operating systems. Linux x86-64 syscall numbers are different from macOS aarch64 syscall numbers. Users should consult platform-specific documentation.

{{ rule(id="9.2:7", cat="example") }}

```rue
fn main() -> i32 {
    checked {
        // Linux x86-64: write(fd=1, buf, len)
        let result = @syscall(1_u64, 1_u64, msg_ptr, msg_len);

        // Linux x86-64: exit_group(code)
        @syscall(231_u64, 0_u64);
    };
    0
}
```
