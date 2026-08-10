use inkwell::basic_block::BasicBlock;
use inkwell::values::{BasicValueEnum, FunctionValue};
use std::collections::{HashMap, HashSet};

pub struct LivenessInfo<'ctx> {
	pub live_in: HashMap<BasicBlock<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
	pub live_out: HashMap<BasicBlock<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
}

pub fn analyze<'ctx>(function: FunctionValue<'ctx>) -> LivenessInfo<'ctx> {
	// 1. per-block uses[] / defs[]
	// 2. fixed-point iteration over the two dataflow equations
	todo!()
}