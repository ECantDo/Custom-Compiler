mod liveness;
mod graph;
pub mod config;

use config::RegisterConfig;
use graph::{Coloring, Graph};
use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicValueEnum, FunctionValue, InstructionValue};

/// Load `registers.cfg` and run the allocator over every defined function
/// in `module`.
pub fn allocate_module<'ctx>(module: &Module<'ctx>) -> Result<(), String> {
	let target = RegisterConfig::load_default()?;

	for function in module.get_functions() {
		if function.count_basic_blocks() == 0 {
			continue; // extern declaration — no body to allocate
		}
		initialize_register_allocation(function, &target);
	}

	Ok(())
}

pub fn initialize_register_allocation<'ctx>(
	function: FunctionValue<'ctx>,
	target: &RegisterConfig,
) -> Option<Coloring<'ctx>> {
	let liveness = liveness::compute_liveness(function);
	let graph = Graph::build_interference_graph(function, &liveness);

	// The number of physical registers we're allowed to colour with is just
	// the size of the allocatable pool in the config.
	let k = target.num_colors();
	let name = function.get_name().to_str().unwrap_or("?");

	match graph.color(k) {
		Ok(coloring) => {
			eprintln!(
				"regalloc {name}: {} value(s) coloured using {} of {k} register(s)",
				coloring.colors.len(),
				coloring.colors_used,
			);
			Some(coloring)
		}
		Err(spills) => panic!(
			"regalloc {name}: {} value(s) spilled — no stack to spill to yet",
			spills.len(),
		),
	}
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

