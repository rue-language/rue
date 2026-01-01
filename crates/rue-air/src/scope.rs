//! Scoped variable tracking trait.
//!
//! This module provides the [`ScopedContext`] trait for types that track
//! scoped variable bindings. This is used by both [`AnalysisContext`] (AIR
//! emission) and [`ConstraintContext`] (constraint generation) to share the
//! common scope management logic.
//!
//! # Scope Management
//!
//! The scope management pattern allows variables to be shadowed within nested
//! scopes (like blocks and function bodies). When a scope is popped, all
//! variables introduced in that scope are removed, and any shadowed variables
//! are restored to their previous values.
//!
//! ```text
//! fn example() {
//!     let x = 1;           // x -> slot0
//!     {
//!         push_scope();
//!         let x = 2;       // shadow: x -> slot1 (old x saved)
//!         let y = 3;       // new: y -> slot2
//!         pop_scope();     // x -> slot0 restored, y removed
//!     }
//!     // x = 1 again
//! }
//! ```

use std::collections::HashMap;

use lasso::Spur;

/// Trait for types that track scoped variable bindings.
///
/// This trait abstracts over the common scope management pattern shared by
/// `AnalysisContext` (used during AIR emission) and `ConstraintContext` (used
/// during constraint generation). Both contexts need to:
///
/// 1. Track local variables in a hashmap
/// 2. Support nested scopes with proper shadowing
/// 3. Restore shadowed variables when scopes are popped
///
/// The associated type `VarInfo` allows each context to store different
/// variable information (e.g., `LocalVar` vs `LocalVarInfo`).
pub trait ScopedContext {
    /// The type of variable information stored for each local.
    type VarInfo: Clone;

    /// Get a reference to the locals map.
    fn locals(&self) -> &HashMap<Spur, Self::VarInfo>;

    /// Get a mutable reference to the locals map.
    fn locals_mut(&mut self) -> &mut HashMap<Spur, Self::VarInfo>;

    /// Get a reference to the scope stack.
    fn scope_stack(&self) -> &[Vec<(Spur, Option<Self::VarInfo>)>];

    /// Get a mutable reference to the scope stack.
    fn scope_stack_mut(&mut self) -> &mut Vec<Vec<(Spur, Option<Self::VarInfo>)>>;

    /// Push a new scope onto the stack.
    ///
    /// This creates a new scope for variable bindings. Any variables added
    /// after this call (via `insert_local`) will be tracked in this scope
    /// and can be cleaned up by calling `pop_scope`.
    fn push_scope(&mut self) {
        // Preallocate for a small number of variables. Most scopes (loop bodies,
        // if/match arms) have 0-2 variables; function bodies have more but are
        // less frequent. 2 is a conservative choice until we have real metrics.
        self.scope_stack_mut().push(Vec::with_capacity(2));
    }

    /// Pop the current scope, restoring any shadowed variables and removing new ones.
    ///
    /// When a scope is popped:
    /// - Variables that were introduced in this scope are removed
    /// - Variables that were shadowed are restored to their previous values
    fn pop_scope(&mut self) {
        if let Some(scope_entries) = self.scope_stack_mut().pop() {
            for (symbol, old_value) in scope_entries {
                match old_value {
                    Some(old_var) => {
                        // Restore the shadowed variable
                        self.locals_mut().insert(symbol, old_var);
                    }
                    None => {
                        // Remove the variable that was added in this scope
                        self.locals_mut().remove(&symbol);
                    }
                }
            }
        }
    }

    /// Insert a local variable, tracking it in the current scope for later cleanup.
    ///
    /// If a variable with the same name already exists, the old value is saved
    /// so it can be restored when the current scope is popped (shadowing).
    fn insert_local(&mut self, symbol: Spur, var: Self::VarInfo) {
        let old_value = self.locals_mut().insert(symbol, var);
        // Track in the current scope (if any) for cleanup on pop
        if let Some(current_scope) = self.scope_stack_mut().last_mut() {
            current_scope.push((symbol, old_value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    /// A minimal implementation of ScopedContext for testing.
    #[derive(Debug)]
    struct TestContext {
        locals: HashMap<Spur, i32>,
        scope_stack: Vec<Vec<(Spur, Option<i32>)>>,
    }

    impl TestContext {
        fn new() -> Self {
            Self {
                locals: HashMap::new(),
                scope_stack: Vec::new(),
            }
        }
    }

    impl ScopedContext for TestContext {
        type VarInfo = i32;

        fn locals(&self) -> &HashMap<Spur, Self::VarInfo> {
            &self.locals
        }

        fn locals_mut(&mut self) -> &mut HashMap<Spur, Self::VarInfo> {
            &mut self.locals
        }

        fn scope_stack(&self) -> &[Vec<(Spur, Option<Self::VarInfo>)>] {
            &self.scope_stack
        }

        fn scope_stack_mut(&mut self) -> &mut Vec<Vec<(Spur, Option<Self::VarInfo>)>> {
            &mut self.scope_stack
        }
    }

    #[test]
    fn test_insert_local_without_scope() {
        let interner = ThreadedRodeo::new();
        let x = interner.get_or_intern("x");

        let mut ctx = TestContext::new();
        ctx.insert_local(x, 42);

        assert_eq!(ctx.locals.get(&x), Some(&42));
        // No scope stack, so scope_stack should be empty
        assert!(ctx.scope_stack.is_empty());
    }

    #[test]
    fn test_push_pop_scope_empty() {
        let mut ctx = TestContext::new();

        ctx.push_scope();
        assert_eq!(ctx.scope_stack.len(), 1);

        ctx.pop_scope();
        assert!(ctx.scope_stack.is_empty());
    }

    #[test]
    fn test_scope_removes_new_variable() {
        let interner = ThreadedRodeo::new();
        let x = interner.get_or_intern("x");

        let mut ctx = TestContext::new();

        ctx.push_scope();
        ctx.insert_local(x, 42);
        assert_eq!(ctx.locals.get(&x), Some(&42));

        ctx.pop_scope();
        // Variable should be removed after pop
        assert!(ctx.locals.get(&x).is_none());
    }

    #[test]
    fn test_scope_restores_shadowed_variable() {
        let interner = ThreadedRodeo::new();
        let x = interner.get_or_intern("x");

        let mut ctx = TestContext::new();

        // Add x = 10 in the outer scope (no scope stack yet)
        ctx.insert_local(x, 10);

        // Push a scope and shadow x
        ctx.push_scope();
        ctx.insert_local(x, 20);
        assert_eq!(ctx.locals.get(&x), Some(&20));

        // Pop the scope - x should be restored to 10
        ctx.pop_scope();
        assert_eq!(ctx.locals.get(&x), Some(&10));
    }

    #[test]
    fn test_nested_scopes() {
        let interner = ThreadedRodeo::new();
        let x = interner.get_or_intern("x");
        let y = interner.get_or_intern("y");
        let z = interner.get_or_intern("z");

        let mut ctx = TestContext::new();

        // Outer: x = 1
        ctx.insert_local(x, 1);

        // Scope 1: shadow x = 2, add y = 3
        ctx.push_scope();
        ctx.insert_local(x, 2);
        ctx.insert_local(y, 3);

        // Scope 2: shadow x = 4, add z = 5
        ctx.push_scope();
        ctx.insert_local(x, 4);
        ctx.insert_local(z, 5);

        assert_eq!(ctx.locals.get(&x), Some(&4));
        assert_eq!(ctx.locals.get(&y), Some(&3));
        assert_eq!(ctx.locals.get(&z), Some(&5));

        // Pop scope 2: x = 2, y = 3, z removed
        ctx.pop_scope();
        assert_eq!(ctx.locals.get(&x), Some(&2));
        assert_eq!(ctx.locals.get(&y), Some(&3));
        assert!(ctx.locals.get(&z).is_none());

        // Pop scope 1: x = 1, y removed
        ctx.pop_scope();
        assert_eq!(ctx.locals.get(&x), Some(&1));
        assert!(ctx.locals.get(&y).is_none());
    }

    #[test]
    fn test_multiple_variables_in_scope() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let c = interner.get_or_intern("c");

        let mut ctx = TestContext::new();

        ctx.push_scope();
        ctx.insert_local(a, 1);
        ctx.insert_local(b, 2);
        ctx.insert_local(c, 3);

        assert_eq!(ctx.locals.len(), 3);

        ctx.pop_scope();
        assert!(ctx.locals.is_empty());
    }
}
