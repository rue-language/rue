include!("backend/backend_root_publications.rs");
include!("backend/cfgs.rs");
include!("backend/codegen_unit_batches.rs");
include!("backend/codegen_units.rs");
include!("backend/object_projection_batches.rs");
include!("backend/object_projections.rs");
include!("backend/optimized_cfg_batches.rs");
include!("backend/optimized_cfgs.rs");
include!("backend/raw_cfg_batches.rs");

pub(super) use register_backend_backend_root_publications;
pub(super) use register_backend_cfgs;
pub(super) use register_backend_codegen_unit_batches;
pub(super) use register_backend_codegen_units;
pub(super) use register_backend_object_projection_batches;
pub(super) use register_backend_object_projections;
pub(super) use register_backend_optimized_cfg_batches;
pub(super) use register_backend_optimized_cfgs;
pub(super) use register_backend_raw_cfg_batches;
