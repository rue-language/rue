use super::*;

impl Cfg {
    pub fn payload_storage_stats(&self) -> CfgPayloadStorageStats {
        let mut stats = CfgPayloadStorageStats {
            value_store_logical_bytes: self.extra.logical_bytes(),
            value_store_capacity_bytes: self.extra.capacity_bytes(),
            call_store_logical_bytes: self.call_args.logical_bytes(),
            call_store_capacity_bytes: self.call_args.capacity_bytes(),
            switch_store_logical_bytes: self.switch_cases.logical_bytes(),
            switch_store_capacity_bytes: self.switch_cases.capacity_bytes(),
            projection_store_logical_bytes: self.projections.logical_bytes(),
            projection_store_capacity_bytes: self.projections.capacity_bytes(),
            ..CfgPayloadStorageStats::default()
        };
        let mut account = |family: usize, extent: u32, element_size: usize| {
            let bytes = extent as usize * element_size;
            stats.family_logical_bytes[family] += bytes;
        };
        for inst in &self.values {
            match &inst.data {
                CfgInstData::Intrinsic { args, .. } => {
                    account(0, args.extent(), std::mem::size_of::<CfgValue>())
                }
                CfgInstData::StructInit { fields, .. } => {
                    account(1, fields.extent(), std::mem::size_of::<CfgValue>())
                }
                CfgInstData::ArrayInit { elements } => {
                    account(2, elements.extent(), std::mem::size_of::<CfgValue>())
                }
                CfgInstData::EnumVariant { payload, .. } => {
                    account(3, payload.extent(), std::mem::size_of::<CfgValue>())
                }
                CfgInstData::Call { args, .. } => {
                    account(7, args.extent(), std::mem::size_of::<CfgCallArg>())
                }
                CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } => {
                    account(
                        9,
                        place.projections.extent(),
                        std::mem::size_of::<Projection>(),
                    )
                }
                _ => {}
            }
        }
        for block in &self.blocks {
            match &block.terminator {
                Terminator::Goto { args, .. } => {
                    account(4, args.extent(), std::mem::size_of::<CfgValue>())
                }
                Terminator::Branch {
                    then_args,
                    else_args,
                    ..
                } => {
                    account(5, then_args.extent(), std::mem::size_of::<CfgValue>());
                    account(6, else_args.extent(), std::mem::size_of::<CfgValue>());
                }
                Terminator::Switch { cases, .. } => {
                    account(8, cases.extent(), std::mem::size_of::<(i64, BlockId)>())
                }
                _ => {}
            }
        }
        stats
    }
}
