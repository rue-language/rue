load("@prelude//rust:cargo_package.bzl", "cargo")

# Default Rust compiler flags for the Rue project
DEFAULT_RUSTC_FLAGS = [
    # Warnings that we want to be errors
    "-Dwarnings",
    "-Drust-2018-idioms",
    "-Drust-2021-compatibility",
    
    # Performance optimizations for debug builds
    "-Copt-level=0",
    "-Cdebuginfo=2",
    
    # Enable useful warnings
    "-Wunused",
    "-Wclippy::all",
]

# Default environment variables for Rust builds
DEFAULT_RUST_ENV = {
    # Enable colored output in terminals
    "RUSTC_FORCE_COLORED_OUTPUT": "1",
    # Enable backtrace for better debugging
    "RUST_BACKTRACE": "1",
}

def _merge_env(env, clippy = False):
    """Merge environment variables with defaults."""
    e = dict(DEFAULT_RUST_ENV)
    if env:
        e.update(env)
    
    if clippy:
        e["RUSTC_WRAPPER"] = "clippy-driver"
    
    return e

def _merge_flags(flags, pedantic = False, optimization = None):
    """Merge rustc flags with defaults."""
    f = list(DEFAULT_RUSTC_FLAGS)
    
    if flags:
        f.extend(flags)
    
    if pedantic:
        f.extend([
            "-Wclippy::pedantic",
            "-Wclippy::nursery",
            "-Wclippy::cargo",
        ])
    
    # Optimization level overrides
    if optimization:
        # Remove default opt-level and replace
        f = [flag for flag in f if not flag.startswith("-Copt-level")]
        f.append("-Copt-level={}".format(optimization))
    
    return f

def _common_rust_attrs(env = None, rustc_flags = None, pedantic = False, optimization = None, clippy = False):
    """Common attributes for all Rust targets."""
    return {
        "env": _merge_env(env, clippy),
        "rustc_flags": _merge_flags(rustc_flags, pedantic, optimization),
    }

# Enhanced Rust library with clippy support
def clippy_rust_library(name, env = None, rustc_flags = None, pedantic = False, optimization = None, **kwargs):
    """
    Create a Rust library with clippy integration.
    
    Args:
        name: Target name
        env: Additional environment variables
        rustc_flags: Additional rustc flags
        pedantic: Enable pedantic clippy lints
        optimization: Optimization level (0, 1, 2, 3, "s", "z")
        **kwargs: Additional arguments passed to rust_library
    """
    attrs = _common_rust_attrs(
        env = env,
        rustc_flags = rustc_flags,
        pedantic = pedantic,
        optimization = optimization,
        clippy = True,
    )
    
    cargo.rust_library(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

# Enhanced Rust binary with optimizations
def rust_binary(name, env = None, rustc_flags = None, optimization = "2", **kwargs):
    """
    Create an optimized Rust binary.
    
    Args:
        name: Target name
        env: Additional environment variables  
        rustc_flags: Additional rustc flags
        optimization: Optimization level (default: "2" for release builds)
        **kwargs: Additional arguments passed to rust_binary
    """
    attrs = _common_rust_attrs(
        env = env,
        rustc_flags = rustc_flags,
        optimization = optimization,
    )
    
    cargo.rust_binary(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

# Test configuration with appropriate settings
def rust_test(name, env = None, rustc_flags = None, **kwargs):
    """
    Create a Rust test with appropriate test settings.
    
    Args:
        name: Target name
        env: Additional environment variables
        rustc_flags: Additional rustc flags  
        **kwargs: Additional arguments passed to rust_test
    """
    test_env = {
        # Test-specific environment variables
        "RUST_TEST_THREADS": "1",  # Deterministic test execution
        "RUST_BACKTRACE": "0",  # Suppress backtraces in tests
        "RUST_LOG": "rue=warn",  # Suppress debug/trace logs in tests
    }
    
    if env:
        test_env.update(env)
    
    attrs = _common_rust_attrs(
        env = test_env,
        rustc_flags = rustc_flags,
        optimization = "0",  # Debug builds for tests
    )
    
    native.rust_test(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

# Benchmark configuration
def rust_benchmark(name, env = None, rustc_flags = None, **kwargs):
    """
    Create a Rust benchmark with optimization settings.
    
    Args:
        name: Target name
        env: Additional environment variables
        rustc_flags: Additional rustc flags
        **kwargs: Additional arguments passed to rust_binary (benchmarks are binaries)
    """
    bench_flags = [
        "-Copt-level=3",  # Maximum optimization for benchmarks
        "-Ctarget-cpu=native",  # Optimize for current CPU
        "-Clto=thin",  # Link-time optimization
    ]
    
    if rustc_flags:
        bench_flags.extend(rustc_flags)
    
    attrs = _common_rust_attrs(
        env = env,
        rustc_flags = bench_flags,
    )
    
    cargo.rust_binary(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

# Development build configuration
def rust_dev_binary(name, env = None, rustc_flags = None, **kwargs):
    """
    Create a Rust binary optimized for development (fast compile times).
    
    Args:
        name: Target name
        env: Additional environment variables
        rustc_flags: Additional rustc flags
        **kwargs: Additional arguments passed to rust_binary
    """
    attrs = _common_rust_attrs(
        env = env,
        rustc_flags = rustc_flags,
        optimization = "0",  # Fast compilation
    )
    
    cargo.rust_binary(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

# Utility function for corpus tests (common in compiler projects)
def rust_corpus_test(name, srcs, corpus_dir, **kwargs):
    """
    Create a corpus test that runs against a directory of test files.
    
    Args:
        name: Target name
        srcs: Test source files
        corpus_dir: Directory containing test corpus files
        **kwargs: Additional arguments
    """
    attrs = _common_rust_attrs(
        env = {
            "CORPUS_DIR": "$(location {})".format(corpus_dir),
        },
    )
    
    cargo.rust_test(
        name = name,
        srcs = srcs,
        resources = [corpus_dir],
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

# Snapshot testing utilities
def rust_snapshot_test(name, env = None, rustc_flags = None, snapshot_dir = None, **kwargs):
    """
    Create a standard Rust snapshot test that fails when snapshots don't match.
    
    Args:
        name: Target name
        env: Additional environment variables
        rustc_flags: Additional rustc flags
        snapshot_dir: Directory containing snapshot files (auto-detected if not provided)
        **kwargs: Additional arguments passed to rust_test
    """
    test_env = {
        "RUST_TEST_THREADS": "1",  # Deterministic test execution
        "RUST_BACKTRACE": "0",  # Suppress backtraces in tests
        "RUST_LOG": "rue=warn",  # Suppress debug/trace logs in tests
    }
    
    if snapshot_dir:
        test_env["RUE_SNAPSHOT_DIR"] = snapshot_dir
    
    if env:
        test_env.update(env)
    
    attrs = _common_rust_attrs(
        env = test_env,
        rustc_flags = rustc_flags,
        optimization = "0",
    )
    
    native.rust_test(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

def rust_snapshot_update(name, env = None, rustc_flags = None, snapshot_dir = None, **kwargs):
    """
    Create a Rust snapshot test target that updates snapshots when run.
    This is equivalent to running the test with UPDATE_SNAPSHOTS=1.
    
    Usage: ./buck2 test //crates/rue-parser:update_snapshots
    
    Args:
        name: Target name (typically "update_snapshots" or similar)
        env: Additional environment variables
        rustc_flags: Additional rustc flags
        snapshot_dir: Directory containing snapshot files (auto-detected if not provided)
        **kwargs: Additional arguments passed to rust_test
    """
    test_env = {
        "UPDATE_SNAPSHOTS": "1",  # Enable snapshot updates
        "RUST_TEST_THREADS": "1",  # Deterministic test execution
        "RUST_BACKTRACE": "0",  # Suppress backtraces in tests
        "RUST_LOG": "rue=warn",  # Suppress debug/trace logs in tests
    }
    
    if snapshot_dir:
        test_env["RUE_SNAPSHOT_DIR"] = snapshot_dir
    
    if env:
        test_env.update(env)
    
    attrs = _common_rust_attrs(
        env = test_env,
        rustc_flags = rustc_flags,
        optimization = "0",
    )
    
    native.rust_test(
        name = name,
        env = attrs["env"],
        rustc_flags = attrs["rustc_flags"],
        **kwargs
    )

def rust_snapshot_tests(name, env = None, rustc_flags = None, snapshot_dir = None, **kwargs):
    """
    Create both regular snapshot tests and snapshot update targets.
    This creates two targets:
    - {name}: Regular test that fails on snapshot mismatch
    - {name}_update: Test that updates snapshots when run
    
    Args:
        name: Base target name
        env: Additional environment variables
        rustc_flags: Additional rustc flags
        snapshot_dir: Directory containing snapshot files (auto-detected if not provided)
        **kwargs: Additional arguments passed to both test targets
    """
    # Regular snapshot test
    rust_snapshot_test(
        name = name,
        env = env,
        rustc_flags = rustc_flags,
        snapshot_dir = snapshot_dir,
        **kwargs
    )
    
    # Snapshot update test
    rust_snapshot_update(
        name = name + "_update",
        env = env,
        rustc_flags = rustc_flags,
        snapshot_dir = snapshot_dir,
        **kwargs
    )
