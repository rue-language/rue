# Rust utility macros for the Rue project

def collect_rust_targets(ctx, scope_pattern, target_name_pattern = None):
    """
    Collect Rust targets from the given scope pattern.
    
    Args:
        ctx: BXL context
        scope_pattern: Query scope (e.g., "//crates/...")
        target_name_pattern: Optional regex pattern for target names
    
    Returns:
        List of target nodes
    """
    # Ensure scope has a valid pattern
    if not scope_pattern.endswith("...") and not ":" in scope_pattern:
        # Add a wildcard to match all targets in the package
        if scope_pattern.endswith("/"):
            scope_pattern = scope_pattern + "..."
        else:
            scope_pattern = scope_pattern + "/..."
    
    if target_name_pattern:
        query = "attrregexfilter(name, '{}', {})".format(target_name_pattern, scope_pattern)
    else:
        query = "kind('rust_.*', '{}')".format(scope_pattern)
    
    return ctx.uquery().eval(query)

def order_targets_by_name(nodes):
    """
    Order target nodes by their string representation for stable output.
    
    Args:
        nodes: List of target nodes
        
    Returns:
        List of target nodes sorted by name
    """
    by_name = {str(node): node for node in nodes}
    sorted_names = sorted(by_name.keys())
    return [by_name[name] for name in sorted_names]

def print_target_summary(ctx, nodes, operation):
    """
    Print a summary of targets being processed.
    
    Args:
        ctx: BXL context
        nodes: List of target nodes
        operation: Description of the operation being performed
    """
    if len(nodes) == 0:
        ctx.output.print("No targets found for {}.".format(operation))
        return False
    
    ctx.output.print("Running {} on {} target(s):".format(operation, len(nodes)))
    for node in nodes[:5]:  # Show first 5 targets
        ctx.output.print("  - {}".format(str(node)))
    
    if len(nodes) > 5:
        ctx.output.print("  ... and {} more".format(len(nodes) - 5))
    
    return True