# Clippy utility macros for the Rue project

def auto_discover_clippy_targets(ctx, root_dir = "crates"):
    """
    Auto-discover clippy targets in the given root directory.
    
    Args:
        ctx: BXL context  
        root_dir: Root directory to search for clippy targets
        
    Returns:
        List of clippy target nodes
    """
    scope = "//{}/*".format(root_dir)
    query = "attrregexfilter(name, '^clippy($|-.+)$', {})".format(scope)
    return ctx.uquery().eval(query)

def clippy_all_group_macro(name, root = "crates", visibility = None, **kwargs):
    """
    Create a group target that includes all clippy targets under the given root.
    
    Args:
        name: Name of the group target
        root: Root directory to search for clippy targets  
        visibility: Target visibility
        **kwargs: Additional arguments passed to the group rule
    """
    # This is a placeholder - the actual implementation would use Buck2's
    # auto-discovery mechanism to create a group of all clippy targets
    pass