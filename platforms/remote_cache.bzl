# Cache-only execution platform (RUE-316).
#
# Same as the default execution platform (local execution), but with the remote
# ACTION CACHE + CAS enabled — so unchanged actions are served from BuildBuddy
# instead of rebuilt, while actions still *execute* locally (no remote execution
# offload). Opt-in: nothing uses this unless `.buckconfig.local` sets
# `[build] execution_platforms = root//platforms:remote_cache` (see
# .buckconfig.local.example).
#
# remote_enabled=False + remote_cache_enabled=True is the cache-only combination:
# reads/writes the remote cache, never dispatches execution remotely. That keeps
# it correctness-neutral (a stale/poisoned entry can only ever be a cache miss's
# worth of wrong; local execution is always the source of truth for new actions).

def _remote_cache_platform_impl(ctx):
    return [
        DefaultInfo(),
        ExecutionPlatformRegistrationInfo(
            platforms = [
                ExecutionPlatformInfo(
                    label = ctx.label.raw_target(),
                    configuration = ConfigurationInfo(constraints = {}, values = {}),
                    executor_config = CommandExecutorConfig(
                        local_enabled = True,
                        remote_enabled = False,
                        remote_cache_enabled = True,
                        remote_execution_use_case = "buck2-default",
                        remote_output_paths = "output_paths",
                        remote_execution_properties = {},
                        use_limited_hybrid = False,
                    ),
                ),
            ],
        ),
    ]

remote_cache_platform = rule(
    impl = _remote_cache_platform_impl,
    attrs = {},
)
