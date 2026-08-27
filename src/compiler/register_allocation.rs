mod liveness;
mod graph;

use graph::Graph;
use inkwell::values::{AnyValue, BasicValueEnum, FunctionValue, InstructionValue};


pub fn initialize_register_allocation<'ctx>(function: FunctionValue<'ctx>) {
	let liveness = liveness::compute_liveness(function);
	let graph = Graph::build_interference_graph(function, &liveness);
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

