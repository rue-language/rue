# Remote execution + cache platform (RUE-316). remote_enabled=True opens the RE
# connection (required even for cache-only use in OSS buck2). Limited hybrid
# prefers remote execution; the repository's ./buck2 wrapper therefore adds
# --prefer-local for ordinary execution commands. Explicit execution-mode flags
# still override it. RUE-320's full-remote platform adds an execution constraint
# that selects the worker's linker driver; the cache-only platform deliberately
# omits it because cache misses still execute with the native toolchain.
def _remote_cache_platform_impl(ctx):
    base = ctx.attrs.base[ExecutionPlatformRegistrationInfo]
    remote_execution = ctx.attrs.remote_execution[ConstraintValueInfo]
    platforms = []
    for p in base.platforms:
        configuration = p.configuration.copy()
        if ctx.attrs.mark_remote_execution:
            configuration.insert(remote_execution)
        platforms.append(ExecutionPlatformInfo(
            label = ctx.label.raw_target(),
            configuration = configuration,
            executor_config = CommandExecutorConfig(
                local_enabled = True,
                remote_enabled = True,
                remote_cache_enabled = True,
                allow_cache_uploads = True,
                use_limited_hybrid = True,
                allow_hybrid_fallbacks_on_failure = ctx.attrs.allow_hybrid_fallbacks_on_failure,
                # A cold Rust graph can exceed BuildBuddy's default per-action
                # memory estimate. Request enough headroom to keep rustc actions
                # out of the executor OOM killer (RUE-320).
                remote_execution_properties = {
                    "EstimatedMemory": "4GB",
                    "OSFamily": "Linux",
                    # Pinned by immutable OCI index digest (RUE-1165). The
                    # merge-group canary's claim is "this compiler builds on the
                    # worker we reviewed"; a republished moving tag would silently
                    # change what that proves, and would turn an upstream image
                    # change into a merge-queue failure with no local repro.
                    # BuildBuddy publishes no versioned tag for this image, only
                    # a moving stream, so the digest stands alone rather than
                    # accompanying a reviewed release tag as the actionlint pin
                    # does. //:required-ci-container-pin-validation rejects a
                    # reference without a digest; the resolution and update
                    # procedure is in docs/process/build-cache.md.
                    #
                    # gcr.io/flame-public/rbe-ubuntu22-04, resolved 2026-07-28.
                    "container-image": "docker://gcr.io/flame-public/rbe-ubuntu22-04@sha256:0d84a80bb0fc36ba5381942adcf6493249594dcc9044845c617b78c9b621cae3",
                },
                remote_execution_use_case = "buck2-default",
                remote_output_paths = "output_paths",
            ),
        ))
    return [
        DefaultInfo(),
        ExecutionPlatformRegistrationInfo(
            platforms = platforms,
            exec_marker_constraint = base.exec_marker_constraint,
        ),
    ]

remote_cache_platform = rule(
    impl = _remote_cache_platform_impl,
    attrs = {
        "base": attrs.dep(providers = [ExecutionPlatformRegistrationInfo], default = "prelude//platforms:default"),
        "allow_hybrid_fallbacks_on_failure": attrs.bool(default = True),
        "mark_remote_execution": attrs.bool(default = False),
        "remote_execution": attrs.dep(providers = [ConstraintValueInfo], default = "root//constraints:remote-execution"),
    },
)
