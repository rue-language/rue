# Remote execution + cache platform (RUE-316). remote_enabled=True opens the RE
# connection (required for the cache in OSS buck2); limited hybrid + local
# fallback keeps most execution local. rustc's shared libs resolve on the remote
# worker via the $ORIGIN RPATH now that the toolchain carries its full tree
# (toolchains/rust/defs.bzl).
def _remote_cache_platform_impl(ctx):
    base = ctx.attrs.base[ExecutionPlatformRegistrationInfo]
    platforms = [
        ExecutionPlatformInfo(
            label = ctx.label.raw_target(),
            configuration = p.configuration,
            executor_config = CommandExecutorConfig(
                local_enabled = True,
                remote_enabled = True,
                remote_cache_enabled = True,
                allow_cache_uploads = True,
                use_limited_hybrid = True,
                allow_hybrid_fallbacks_on_failure = True,
                remote_execution_properties = {"OSFamily": "Linux", "container-image": "docker://gcr.io/flame-public/rbe-ubuntu22-04:latest"},
                remote_execution_use_case = "buck2-default",
                remote_output_paths = "output_paths",
            ),
        )
        for p in base.platforms
    ]
    return [DefaultInfo(), ExecutionPlatformRegistrationInfo(platforms = platforms)]

remote_cache_platform = rule(
    impl = _remote_cache_platform_impl,
    attrs = {"base": attrs.dep(providers = [ExecutionPlatformRegistrationInfo], default = "prelude//platforms:default")},
)
