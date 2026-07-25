//! Clone-from-template feasibility probe (RUE-1133; RUE-1091 ordered probe #1).
//!
//! # Why this exists
//!
//! Every reached body currently rebuilds the whole declaration epoch before it
//! analyzes one body: `prepare_query_declaration_shells`,
//! `project_durable_declaration_semantics`, the durable install, and the stable
//! endpoint installs. That prefix is O(declarations) per body and, on the
//! measured programs, most of the cold compile.
//!
//! RUE-1091's first ordered probe asks one question about that prefix: **how
//! much of it is derivation, and how much is structure that any sharing scheme
//! still has to move?** This module answers it by building the bound epoch once
//! per revision and *deep-copying* it per body. The copy is still
//! O(declarations × bodies) — it replaces derivation with copying and nothing
//! else — so the difference between the two arms is the derivation cost, and
//! what remains in the clone arm is the structural floor.
//!
//! # What this is not
//!
//! This is not a production boundary and cannot become one. A cheap clone is
//! performance evidence only: it narrows no dependency, so it proves nothing
//! about exact invalidation and cannot complete RUE-1091. Nothing enables it by
//! default; a caller must ask for it explicitly through
//! [`crate::unstable::enable_clone_from_template_probe`], which only the
//! measurement harnesses do.
//!
//! # Honest counters
//!
//! The ordinary per-body prepare/project/install/endpoint counters are unchanged
//! and keep being charged at their own stage sources. In the clone arm those
//! stages genuinely run once per revision rather than once per body, so those
//! counters fall — because the work moved into a copy, which
//! [`CloneProbeMetrics`] then reports in full. Reading the two together is the
//! point: neither is allowed to hide the other.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rue_air::{EpochCloneUnits, PerBodyDeclarationContextWork};

use crate::body_query::{BodyQueryKey, BodyTransaction, WellKnownOptionResolution};
use crate::canonical_semantic::{BoundBodyEpoch, analyze_body_in_epoch, build_bound_body_epoch};
use crate::durable_semantics::{DurableAnonymousNominal, DurableDeclarationSemantic};
use crate::{CanonicalImportGraph, CanonicalMergedProgram, CanonicalRirOutput, CompileOptions};

/// How the probe treats each reached body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneProbeMode {
    /// Analyze every body from a clone of the retained template. This is the
    /// timing arm: the per-body prefix runs once per revision, and each body
    /// pays a copy instead.
    CloneOnly,
    /// Analyze every body twice — once by rebuilding the epoch exactly as
    /// production does, once from a template clone — and compare the published
    /// transactions. This is the parity arm: it establishes that the clone
    /// publishes the same artifacts, diagnostics, references and producer
    /// identities. It is deliberately useless for timing.
    Differential,
}

/// An owned snapshot of the probe meter (RUE-1091 probe #1 "Record
/// separately"). Timings are wall clock over the exact operation named; unit
/// counts come from [`EpochCloneUnits`], which reads the copied containers
/// themselves rather than any input slice length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloneProbeMetrics {
    /// Bodies the probe served.
    pub bodies_analyzed: u64,
    /// Templates derived from scratch. One per revision is the intended result;
    /// more means the per-body epoch inputs differed and the retained template
    /// could not serve the body (see `template_input_misses`).
    pub templates_built: u64,
    /// Template copies taken.
    pub templates_cloned: u64,
    /// Bodies whose per-body epoch inputs — the materialized anonymous nominals
    /// and the well-known `Option` registry, both installed *into* the epoch —
    /// differed from the retained template's, forcing a rebuild before the
    /// clone. A non-zero value bounds how much of a program one per-revision
    /// template can actually serve.
    pub template_input_misses: u64,
    /// Time spent deriving templates.
    pub template_build_nanos: u64,
    /// Time spent copying templates.
    pub clone_nanos: u64,
    /// Time spent in body analysis and publication, in the clone arm.
    pub analyze_nanos: u64,
    /// Time spent rebuilding the epoch the production way, including that arm's
    /// own analysis and publication. Non-zero only in
    /// [`CloneProbeMode::Differential`].
    pub rebuild_nanos: u64,
    /// Bodies whose two arms were compared.
    pub parity_comparisons: u64,
    /// Bodies whose two arms published different transactions. Must be zero: a
    /// non-zero value means the clone is not a faithful stand-in for a derived
    /// epoch and the probe's timing result is meaningless.
    pub parity_mismatches: u64,
    /// Structural units copied, by family.
    pub units: EpochCloneUnits,
}

/// The session-held probe meter. Lives behind an `Arc` like the recipe-cache and
/// overlay meters so a request-scoped probe can charge it without borrowing the
/// session, and reads zero on every compile that never enabled the probe.
#[derive(Debug, Default)]
pub(crate) struct CloneProbeMeter {
    bodies_analyzed: AtomicU64,
    templates_built: AtomicU64,
    templates_cloned: AtomicU64,
    template_input_misses: AtomicU64,
    template_build_nanos: AtomicU64,
    clone_nanos: AtomicU64,
    analyze_nanos: AtomicU64,
    rebuild_nanos: AtomicU64,
    parity_comparisons: AtomicU64,
    parity_mismatches: AtomicU64,
    declaration_namespace_entries_copied: AtomicU64,
    type_pool_entries_copied: AtomicU64,
    parameter_entries_copied: AtomicU64,
    module_registry_entries_copied: AtomicU64,
    endpoint_entries_copied: AtomicU64,
}

impl CloneProbeMeter {
    pub(crate) fn snapshot(&self) -> CloneProbeMetrics {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        CloneProbeMetrics {
            bodies_analyzed: load(&self.bodies_analyzed),
            templates_built: load(&self.templates_built),
            templates_cloned: load(&self.templates_cloned),
            template_input_misses: load(&self.template_input_misses),
            template_build_nanos: load(&self.template_build_nanos),
            clone_nanos: load(&self.clone_nanos),
            analyze_nanos: load(&self.analyze_nanos),
            rebuild_nanos: load(&self.rebuild_nanos),
            parity_comparisons: load(&self.parity_comparisons),
            parity_mismatches: load(&self.parity_mismatches),
            units: EpochCloneUnits {
                templates_built: load(&self.templates_built),
                templates_cloned: load(&self.templates_cloned),
                declaration_namespace_entries_copied: load(
                    &self.declaration_namespace_entries_copied,
                ),
                type_pool_entries_copied: load(&self.type_pool_entries_copied),
                parameter_entries_copied: load(&self.parameter_entries_copied),
                module_registry_entries_copied: load(&self.module_registry_entries_copied),
                endpoint_entries_copied: load(&self.endpoint_entries_copied),
            },
        }
    }

    fn charge_nanos(counter: &AtomicU64, elapsed: Duration) {
        counter.fetch_add(
            u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    fn charge_units(&self, units: EpochCloneUnits) {
        self.declaration_namespace_entries_copied.fetch_add(
            units.declaration_namespace_entries_copied,
            Ordering::Relaxed,
        );
        self.type_pool_entries_copied
            .fetch_add(units.type_pool_entries_copied, Ordering::Relaxed);
        self.parameter_entries_copied
            .fetch_add(units.parameter_entries_copied, Ordering::Relaxed);
        self.module_registry_entries_copied
            .fetch_add(units.module_registry_entries_copied, Ordering::Relaxed);
        self.endpoint_entries_copied
            .fetch_add(units.endpoint_entries_copied, Ordering::Relaxed);
    }
}

/// The per-body inputs an epoch is built against. A retained template can serve
/// a body only when these compare equal: they are installed *into* the epoch, so
/// an epoch built against different ones is a different epoch.
#[derive(Debug, PartialEq, Eq)]
struct TemplateInputs {
    anonymous_nominals: Vec<DurableAnonymousNominal>,
    well_known: WellKnownOptionResolution,
}

struct RetainedTemplate<'a> {
    inputs: TemplateInputs,
    epoch: BoundBodyEpoch<'a>,
}

/// One request-scoped clone-from-template probe. Retains at most one bound
/// epoch and serves each reached body from a copy of it.
pub(crate) struct CloneFromTemplateProbe<'a> {
    mode: CloneProbeMode,
    meter: std::sync::Arc<CloneProbeMeter>,
    template: Option<RetainedTemplate<'a>>,
}

impl<'a> CloneFromTemplateProbe<'a> {
    pub(crate) fn new(mode: CloneProbeMode, meter: std::sync::Arc<CloneProbeMeter>) -> Self {
        Self {
            mode,
            meter,
            template: None,
        }
    }

    /// Analyze one body through the probe, in place of
    /// [`crate::canonical_semantic::analyze_body_query`].
    ///
    /// `work` is the ordinary per-body declaration-context accounting. It is
    /// charged only by the stages that actually run, so a body served from a
    /// clone contributes nothing to it — precisely the observation the probe
    /// exists to make, and why the probe's own counters must be read beside it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn analyze_body(
        &mut self,
        merged: &CanonicalMergedProgram,
        rir: &'a CanonicalRirOutput,
        options: &CompileOptions,
        imports: &CanonicalImportGraph,
        query_shells: &[rue_air::SemanticDeclarationShell],
        query_declarations: &[DurableDeclarationSemantic],
        query_anonymous_nominals: &[DurableAnonymousNominal],
        well_known: &WellKnownOptionResolution,
        key: &BodyQueryKey,
        cancellation: &rue_query::CancellationToken,
        work: &mut PerBodyDeclarationContextWork,
    ) -> Result<BodyTransaction, rue_query::QueryAbort> {
        if cancellation.is_canceled() {
            return Err(rue_query::QueryAbort::Canceled);
        }
        self.meter.bodies_analyzed.fetch_add(1, Ordering::Relaxed);

        // The parity arm rebuilds the epoch exactly as production does first, so
        // the comparison is against a genuinely independent derivation rather
        // than a second read of the same copy.
        let rebuilt = if self.mode == CloneProbeMode::Differential {
            let started = Instant::now();
            let mut rebuild_work = PerBodyDeclarationContextWork::default();
            let epoch = build_bound_body_epoch(
                merged,
                rir,
                options,
                imports,
                query_shells,
                query_declarations,
                query_anonymous_nominals,
                well_known,
                &key.instance,
                &mut rebuild_work,
            );
            let transaction = match epoch {
                Ok(epoch) => analyze_body_in_epoch(epoch, merged, key, cancellation)?,
                Err(failure) => failure,
            };
            CloneProbeMeter::charge_nanos(&self.meter.rebuild_nanos, started.elapsed());
            // The rebuild arm performs the ordinary stages, so it is the arm
            // that charges the ordinary counters. In the clone-only arm the
            // template build below charges them once per revision instead.
            absorb_work(work, &rebuild_work);
            Some(transaction)
        } else {
            None
        };

        let inputs = TemplateInputs {
            anonymous_nominals: query_anonymous_nominals.to_vec(),
            well_known: well_known.clone(),
        };
        if self
            .template
            .as_ref()
            .is_none_or(|template| template.inputs != inputs)
        {
            if self.template.is_some() {
                self.meter
                    .template_input_misses
                    .fetch_add(1, Ordering::Relaxed);
            }
            let started = Instant::now();
            let mut template_work = PerBodyDeclarationContextWork::default();
            let epoch = build_bound_body_epoch(
                merged,
                rir,
                options,
                imports,
                query_shells,
                query_declarations,
                query_anonymous_nominals,
                well_known,
                &key.instance,
                &mut template_work,
            );
            CloneProbeMeter::charge_nanos(&self.meter.template_build_nanos, started.elapsed());
            let epoch = match epoch {
                Ok(epoch) => epoch,
                // A template that cannot be derived is a deterministic stage
                // failure for this body, exactly as in production. Nothing is
                // retained, so the next body re-attempts the derivation.
                Err(failure) => return Ok(rebuilt.unwrap_or(failure)),
            };
            self.meter.templates_built.fetch_add(1, Ordering::Relaxed);
            if self.mode == CloneProbeMode::CloneOnly {
                // The per-revision derivation really happened, so the ordinary
                // stage counters record it — once, not once per body.
                absorb_work(work, &template_work);
            }
            self.template = Some(RetainedTemplate { inputs, epoch });
        }

        let template = self
            .template
            .as_ref()
            .expect("a template was just retained");
        let mut units = EpochCloneUnits::default();
        let started = Instant::now();
        let clone = template.epoch.clone_from_template(&mut units);
        CloneProbeMeter::charge_nanos(&self.meter.clone_nanos, started.elapsed());
        self.meter.templates_cloned.fetch_add(1, Ordering::Relaxed);
        self.meter.charge_units(units);

        let started = Instant::now();
        let cloned_transaction = analyze_body_in_epoch(clone, merged, key, cancellation)?;
        CloneProbeMeter::charge_nanos(&self.meter.analyze_nanos, started.elapsed());

        if let Some(rebuilt) = rebuilt {
            self.meter
                .parity_comparisons
                .fetch_add(1, Ordering::Relaxed);
            if !crate::body_query::transaction_equal(&rebuilt, &cloned_transaction) {
                self.meter.parity_mismatches.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(cloned_transaction)
    }
}

/// Fold stage-sourced per-body declaration-context work into a running total.
fn absorb_work(total: &mut PerBodyDeclarationContextWork, stage: &PerBodyDeclarationContextWork) {
    total.cold_body_preparations += stage.cold_body_preparations;
    total.declarations_inspected += stage.declarations_inspected;
    total.modules_registered += stage.modules_registered;
    total.rir_indexes_constructed += stage.rir_indexes_constructed;
    total.rir_instructions_visited += stage.rir_instructions_visited;
    total.shells_prepared += stage.shells_prepared;
    total.semantics_installed += stage.semantics_installed;
    total.projections_performed += stage.projections_performed;
    total.durable_source_records_inspected += stage.durable_source_records_inspected;
    total.durable_source_records_copied += stage.durable_source_records_copied;
    total.endpoints_installed += stage.endpoints_installed;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompileOptions, CompilerSession, SourceSnapshot};

    /// A corpus with several unrelated declarations and several reached bodies,
    /// so a per-body epoch is genuinely O(declarations) and the two arms have
    /// something to disagree about.
    fn corpus(bodies: usize, decls: usize) -> SourceSnapshot {
        let mut source = String::new();
        for index in 0..bodies {
            source.push_str(&format!("fn b{index}() -> i32 {{ {index} }}\n"));
        }
        for index in 0..decls {
            source.push_str(&format!("fn d{index}() -> i32 {{ {index} }}\n"));
        }
        source.push_str("fn main() -> i32 {\n    let mut acc = 0;\n");
        for index in 0..bodies {
            source.push_str(&format!("    acc = acc + b{index}();\n"));
        }
        source.push_str("    acc\n}\n");
        SourceSnapshot::single("main.rue", source).expect("synthetic corpus parses")
    }

    fn compile(
        snapshot: &SourceSnapshot,
        mode: Option<CloneProbeMode>,
    ) -> (crate::CanonicalSemanticWork, CloneProbeMetrics) {
        let mut session = CompilerSession::new();
        session.enable_clone_from_template_probe(mode);
        session.update(snapshot).into_result().unwrap();
        let output = session
            .canonical_semantic(&CompileOptions::default())
            .expect("synthetic corpus compiles");
        let work = output.work();
        (work, session.clone_from_template_probe_metrics())
    }

    // The whole point of the probe: a body analyzed from a copy of a template
    // publishes the same transaction as a body analyzed from a freshly derived
    // epoch. `transaction_equal` compares the canonical body, references,
    // produced anonymous nominals, and the rendered diagnostic stream, so a
    // divergence in artifacts, identities, or diagnostic ORDER fails here.
    //
    // Without this, the probe's timing result would be worthless: a clone that
    // published something else would simply be measuring a different program.
    #[test]
    fn a_cloned_epoch_publishes_what_a_derived_epoch_publishes() {
        let snapshot = corpus(6, 12);
        let (work, metrics) = compile(&snapshot, Some(CloneProbeMode::Differential));
        assert!(
            metrics.parity_comparisons >= 7,
            "every reached body must be compared, got {metrics:?}"
        );
        assert_eq!(
            metrics.parity_mismatches, 0,
            "a clone-arm body diverged from the rebuild arm: {metrics:?}"
        );
        assert_eq!(
            work.body_analysis.bodies_succeeded, metrics.parity_comparisons as usize,
            "every succeeding body went through the probe"
        );
    }

    // One template serves an entire revision on a program with no per-body epoch
    // inputs. This is the probe's premise: if the retained template could not be
    // reused, "build once per revision" would not describe what ran.
    #[test]
    fn one_template_serves_every_body_of_a_revision() {
        let snapshot = corpus(8, 16);
        let (_, metrics) = compile(&snapshot, Some(CloneProbeMode::CloneOnly));
        assert_eq!(metrics.templates_built, 1, "{metrics:?}");
        assert_eq!(metrics.template_input_misses, 0, "{metrics:?}");
        assert_eq!(
            metrics.templates_cloned, metrics.bodies_analyzed,
            "each body is served from exactly one copy: {metrics:?}"
        );
        assert!(metrics.bodies_analyzed >= 9, "{metrics:?}");
    }

    // The ordinary per-body declaration-context counters stay honest. In the
    // clone arm the prepare/project/install/endpoint stages really do run once
    // per revision instead of once per body, so those counters record ONE
    // prefix — they are not reclassified, hidden, or left reading a per-body
    // total the compiler no longer performs. The copied units then account for
    // what replaced them, and the two must be read together.
    #[test]
    fn the_clone_arm_charges_one_prefix_and_reports_the_copy_that_replaced_it() {
        let snapshot = corpus(8, 16);
        let (rebuild, rebuild_metrics) = compile(&snapshot, None);
        let (clone, clone_metrics) = compile(&snapshot, Some(CloneProbeMode::CloneOnly));

        assert_eq!(
            rebuild_metrics,
            CloneProbeMetrics::default(),
            "an ordinary compile never charges the probe meter"
        );

        let rebuild_context = &rebuild.body_analysis.per_body_declaration_context;
        let clone_context = &clone.body_analysis.per_body_declaration_context;
        assert_eq!(
            clone_context.cold_body_preparations, 1,
            "the clone arm prepares exactly one epoch for the revision"
        );
        assert_eq!(
            rebuild_context.cold_body_preparations, clone_metrics.bodies_analyzed as usize,
            "the rebuild arm prepares one epoch per reached body"
        );
        assert!(
            clone_context.shells_prepared * 2 < rebuild_context.shells_prepared,
            "the clone arm must not still be charging a per-body prefix: \
             clone={clone_context:?} rebuild={rebuild_context:?}"
        );

        // The work did not vanish, it changed shape: the probe reports every
        // unit the copies moved, and the per-body copy is O(declarations).
        assert!(
            clone_metrics.units.total_units_copied() > 0,
            "{clone_metrics:?}"
        );
        assert_eq!(
            clone_metrics.units.templates_cloned, clone_metrics.bodies_analyzed,
            "{clone_metrics:?}"
        );
        assert_eq!(
            clone_metrics.units.declaration_namespace_entries_copied
                % clone_metrics.templates_cloned,
            0,
            "every clone copies the same template, so the namespace total is a \
             clean multiple of the clone count: {clone_metrics:?}"
        );

        // Real body work is unchanged: the same bodies were analyzed and the
        // same AIR came out.
        assert_eq!(
            clone.body_analysis.bodies_succeeded,
            rebuild.body_analysis.bodies_succeeded
        );
        assert_eq!(
            clone.body_analysis.air_instructions_produced,
            rebuild.body_analysis.air_instructions_produced
        );
    }

    // The copy is O(declarations) per body, which is the probe's whole finding:
    // it replaces derivation with copying and does not change the shape. Doubling
    // the unrelated-declaration universe at a fixed body count must roughly
    // double the units each body copies.
    #[test]
    fn the_per_body_copy_grows_with_the_declaration_universe() {
        let per_body = |decls: usize| {
            let (_, metrics) = compile(&corpus(6, decls), Some(CloneProbeMode::CloneOnly));
            assert!(metrics.templates_cloned > 0, "{metrics:?}");
            metrics.units.total_units_copied() / metrics.templates_cloned
        };
        let small = per_body(10);
        let large = per_body(40);
        assert!(
            large > small * 2,
            "a four-fold declaration universe must copy far more per body: \
             small={small} large={large}"
        );
    }

    // A compile that never asks for the probe never touches it. This is the
    // guard that keeps the probe out of production: the meter reads zero and the
    // ordinary path is the one that ran.
    #[test]
    fn a_production_compile_never_enters_the_probe() {
        let snapshot = corpus(4, 8);
        let (work, metrics) = compile(&snapshot, None);
        assert_eq!(metrics, CloneProbeMetrics::default());
        assert!(work.body_analysis.bodies_succeeded > 0);
    }
    // How far one per-revision template goes on a program whose bodies
    // materialize producer nominals. The materialized anonymous nominals are
    // installed INTO the epoch, so this is the shape most likely to make a
    // retained template invalid for a later body — and the case where the
    // probe's "build once per revision" premise would quietly stop describing
    // what ran. It holds here: one template, no input misses, and the anonymous
    // members published identically from a copy.
    #[test]
    fn producer_bodies_still_share_one_template() {
        let source = "\
fn Pair() -> type { struct { a: i32, b: i32 } }
fn Trip() -> type { struct { a: i32, b: i32, c: i32 } }
fn use_pair() -> i32 { let P = Pair(); let p: P = P { a: 1, b: 2 }; p.a + p.b }
fn use_trip() -> i32 { let T = Trip(); let t: T = T { a: 1, b: 2, c: 3 }; t.a + t.b + t.c }
fn main() -> i32 { use_pair() + use_trip() }
";
        let snapshot = SourceSnapshot::single("main.rue", source).unwrap();
        let (work, metrics) = compile(&snapshot, Some(CloneProbeMode::Differential));
        assert!(
            work.body_analysis.bodies_succeeded >= 5,
            "the producer corpus must reach the anonymous members: {metrics:?}"
        );
        assert_eq!(metrics.parity_mismatches, 0, "{metrics:?}");
        assert_eq!(metrics.templates_built, 1, "{metrics:?}");
        assert_eq!(metrics.template_input_misses, 0, "{metrics:?}");
        assert_eq!(
            metrics.parity_comparisons, metrics.bodies_analyzed,
            "{metrics:?}"
        );
    }
}
