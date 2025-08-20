# Formatting utility macros for the Rue project

def collect_rust_sources(ctx, scope_pattern):
    """
    Collect all Rust source files from the given scope pattern.
    
    Args:
        ctx: BXL context
        scope_pattern: Query scope (e.g., "//crates/...")
        
    Returns:
        List of source file paths
    """
    # Query for all rust targets and extract their sources
    rust_targets = ctx.uquery().eval("kind('rust_.*', {})".format(scope_pattern))
    
    source_files = []
    for target in rust_targets:
        # Get target attributes and extract source files
        attrs = ctx.uquery().attrs(target, ["srcs", "source"])
        if "srcs" in attrs:
            for src in attrs["srcs"]:
                if str(src).endswith(".rs"):
                    source_files.append(str(src))
        elif "source" in attrs:
            if str(attrs["source"]).endswith(".rs"):
                source_files.append(str(attrs["source"]))
    
    return list(set(source_files))  # Remove duplicates

def rustfmt_check_command():
    """
    Return the command line for rustfmt check.
    """
    return ["rustfmt", "--check", "--color=always"]

def rustfmt_fix_command():
    """
    Return the command line for rustfmt fix.
    """
    return ["rustfmt", "--color=always"]