# Planning Features

This document describes how to plan new features for Rue.

## When to Plan

Plan before implementing when:
- Adding new language features
- Making significant compiler changes
- The scope isn't immediately clear

Skip formal planning for:
- Bug fixes with obvious solutions
- Documentation updates
- Simple refactoring

## Step 1: Understand the Feature

Before planning, ensure you understand:

1. **What problem does this solve?** - Be specific about the use case
2. **What's the desired behavior?** - How should it work from a user's perspective
3. **What exists already?** - Check for related code, existing issues (Linear `list_issues` in the Rue team)

Ask clarifying questions if requirements are ambiguous.

## Step 2: Assess Feature Size

### Small Features

Characteristics:
- Touches 1-3 files
- Single concept (new operator, syntax sugar, simple addition)
- No new runtime functions
- No new IR instruction kinds
- Completable in one focused session

Examples: Add `%` operator, add `else if` syntax, add unary `+`, new warning type

**Output**: A Linear issue

### Large Features

Characteristics:
- Touches many files across multiple crates
- Multiple implementation phases
- New runtime support functions
- New IR instruction kinds
- New type system concepts
- May span multiple sessions

Examples: Mutable strings, inout parameters, enums and pattern matching, trait system

**Output**: An ADR + Linear epic with sub-issues

## Step 3: Create the Plan

### For Small Features

Create a Linear issue: use the Linear MCP `save_issue` tool with `team: "Rue"`, the `feature` label, and priority 3 (Medium).

Include in the description:
- What the feature does
- Which files need changes
- What tests are needed

### For Large Features

1. **Create an ADR**

   Copy the template and fill it in:
   ```bash
   cp docs/designs/0000-template.md docs/designs/NNNN-<feature>.md
   ```

   Use the next available number. See [../designs/README.md](../designs/README.md) for the full ADR guide.

   Key sections to complete:
   - **Summary**: One paragraph explaining the decision
   - **Context**: Why this is needed
   - **Decision**: Technical details of the approach
   - **Implementation Phases**: Break into independently-committable chunks
   - **Consequences**: Trade-offs and implications

2. **Add the preview feature**

   In `crates/rue-error/src/lib.rs`, add to the `PreviewFeature` enum:
   ```rust
   pub enum PreviewFeature {
       // ...existing...
       YourFeature,
   }
   ```

3. **Create the Linear epic and sub-issues**

   Use the Linear MCP `save_issue` tool:
   - Create the epic: `team: "Rue"`, the `feature` label, priority 3 (Medium)
   - Create a sub-issue per phase ("Phase 1: <description>", "Phase 2: <description>", ...) with the `task` label and `parentId` set to the epic

4. **Update the ADR with issue IDs**

   Fill in the Linear issue IDs in the Implementation Phases section:
   ```markdown
   ## Implementation Phases

   - [ ] **Phase 1: Core parsing** - RUE-42
   - [ ] **Phase 2: Type checking** - RUE-43
   ```

## Step 4: Get Approval

Before implementing, confirm the plan with stakeholders:
- Does the scope look correct?
- Are the phases reasonable?
- Any concerns about the approach?

For large features, the ADR serves as the approval artifact.

## Output Summary

| Feature Size | Artifacts Created |
|--------------|-------------------|
| Small | Linear issue |
| Large | ADR + Linear epic + sub-issues + PreviewFeature entry |

## Next Steps

Once the plan is approved:
- For small features: implement the Linear issue
- For large features: implement the first sub-issue

See [implementation.md](implementation.md) for the implementation process.
