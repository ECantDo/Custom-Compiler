use crate::compiler::register_allocation::instruction_def;
use crate::compiler::register_allocation::liveness::InstructionLiveness;
use inkwell::values::{BasicValueEnum, FunctionValue};
use std::collections::{HashMap, HashSet};

pub struct Graph<'ctx> {
	links: HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
}

impl<'ctx> Graph<'ctx> {
	pub fn build_interference_graph(
		function: FunctionValue<'ctx>,
		liveness: &InstructionLiveness<'ctx>,
	) -> Self {
		let mut graph: HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>> = HashMap::new();

		for block in function.get_basic_block_iter() {
			for instr in block.get_instructions() {
				let Some(def_val) = instruction_def(instr) else { continue; };
				let Some(live_out) = liveness.live_out.get(&instr) else { continue; };

				for &other in live_out {
					if other == def_val { continue; }
					graph.entry(def_val).or_default().insert(other);
					graph.entry(other).or_default().insert(def_val);
				}
			}
		}

		Self {
			links: graph
		}
	}
}
