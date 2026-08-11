use inkwell::values::{BasicValueEnum, InstructionValue};
use std::collections::{HashMap, HashSet};

mod liveness;


fn build_interference_graph<'ctx>(
	def_use_table: &HashMap<InstructionValue<'ctx>, crate::compiler::register_allocation::liveness::DefUseEntry<'ctx>>,
	instr_live_out: &HashMap<InstructionValue<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
) -> HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>> {
	let mut graph: HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>> = HashMap::new();

	for (&instr, entry) in def_use_table {
		let Some(def_val) = entry.def else { continue; };
		let live_out = &instr_live_out[&instr];

		for &other in live_out {
			if other == def_val { continue; }
			graph.entry(def_val).or_default().insert(other);
			graph.entry(other).or_default().insert(def_val);
		}
	}

	graph
}