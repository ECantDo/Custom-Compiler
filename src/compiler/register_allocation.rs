mod liveness;
mod graph;
pub mod config;

use config::RegisterConfig;
pub use graph::Coloring;
use graph::Graph;
use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicValueEnum, FunctionValue, InstructionValue};

/// Load `registers.cfg` and run the allocator over every defined function
/// in `module`, discarding the results. Useful as a standalone
/// "does the whole module colour?" check.
#[allow(dead_code)]
pub fn allocate_module(module: &Module) -> Result<(), String> {
	let target = RegisterConfig::load_default()?;

	for function in module.get_functions() {
		if function.count_basic_blocks() == 0 {
			continue; // extern declaration — no body to allocate
		}
		allocate_function(function, &target)?;
	}

	Ok(())
}

/// Compute a register colouring for `function`: every SSA value that needs a
/// machine register (see [`is_register_value`]) is assigned one of the
/// `target`'s allocatable registers.
///
/// Constants and function parameters are deliberately *not* coloured —
/// constants are rematerialised at each use by the code generator, and
/// parameters live in their ABI argument registers.
///
/// `Err` means some value could not be coloured. There is nowhere to spill
/// to yet (no stack frame), so the caller treats this as a hard error.
pub fn allocate_function<'ctx>(
	function: FunctionValue<'ctx>,
	target: &RegisterConfig,
) -> Result<Coloring<'ctx>, String> {
	let liveness = liveness::compute_liveness(function);
	let graph = Graph::build_interference_graph(function, &liveness);

	let k = target.num_colors();
	let name = function.get_name().to_str().unwrap_or("?");

	graph.color(k).map_err(|spills| {
		format!(
			"regalloc {name}: {} value(s) spilled — only {k} register(s) available \
			 and spilling to the stack is not implemented yet",
			spills.len(),
		)
	})
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

/// Whether `value` is something the allocator hands a register to: an
/// instruction result, as opposed to a constant (rematerialised at each
/// use) or a function parameter (lives in its argument register). Only
/// these appear as nodes in the interference graph.
pub fn is_register_value(value: BasicValueEnum) -> bool {
	match value {
		BasicValueEnum::IntValue(v) => v.as_instruction().is_some(),
		BasicValueEnum::PointerValue(v) => v.as_instruction().is_some(),
		BasicValueEnum::FloatValue(v) => v.as_instruction().is_some(),
		// Aggregates/vectors never reach the allocator on this scalar target.
		_ => false,
	}
}
