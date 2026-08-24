use inkwell::values::{AnyValue, BasicValueEnum, FunctionValue, InstructionValue};
use std::collections::{HashMap, HashSet};

mod liveness;

use liveness::InstructionLiveness;

pub fn initialize_register_allocation<'ctx>(function: FunctionValue<'ctx>) {
	let liveness = liveness::compute_liveness(function);
	let graph = build_interference_graph(function, &liveness);
}

/// Same "does this instruction produce a usable value" check as
/// `compute_def_uses` in liveness.rs — duplicated here rather than shared,
/// since it's a one-liner and pulling in the whole DefUseEntry/uses set
/// would be overkill just to get `def`.
fn instruction_def<'ctx>(instr: InstructionValue<'ctx>) -> Option<BasicValueEnum<'ctx>> {
	if instr.get_type().is_void_type() {
		None
	} else {
		Some(instr.as_any_value_enum().try_into().unwrap())
	}
}

fn build_interference_graph<'ctx>(
	function: FunctionValue<'ctx>,
	liveness: &InstructionLiveness<'ctx>,
) -> HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>> {
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

	graph
}