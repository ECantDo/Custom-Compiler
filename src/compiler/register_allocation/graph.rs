use crate::compiler::register_allocation::config::RegisterConfig;
use crate::compiler::register_allocation::instruction_def;
use crate::compiler::register_allocation::liveness::InstructionLiveness;
use inkwell::values::{BasicValueEnum, FunctionValue};
use std::collections::{HashMap, HashSet};

pub struct Graph<'ctx> {
	links: HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
	order: Vec<BasicValueEnum<'ctx>>,
}

impl<'ctx> Graph<'ctx> {
	pub fn build_interference_graph(
		function: FunctionValue<'ctx>,
		liveness: &InstructionLiveness<'ctx>,
	) -> Self {
		let mut links: HashMap<BasicValueEnum<'ctx>, HashSet<BasicValueEnum<'ctx>>> = HashMap::new();
		let mut order: Vec<BasicValueEnum<'ctx>> = Vec::new();

		let mut add_node = |links: &mut HashMap<_, _>, order: &mut Vec<_>, v: BasicValueEnum<'ctx>| {
			if !links.contains_key(&v) {
				links.insert(v, HashSet::new());
				order.push(v);
			}
		};

		for block in function.get_basic_block_iter() {
			for instr in block.get_instructions() {
				let Some(def_val) = instruction_def(instr) else { continue; };
				let Some(live_out) = liveness.live_out.get(&instr) else { continue; };

				// A value that is defined but interferes with nothing still
				// needs a register, so register it as an (edge-less) node.
				add_node(&mut links, &mut order, def_val);

				for &other in live_out {
					if other == def_val { continue; }
					add_node(&mut links, &mut order, def_val);
					add_node(&mut links, &mut order, other);
					links.get_mut(&def_val).unwrap().insert(other);
					links.get_mut(&other).unwrap().insert(def_val);
				}
			}
		}

		Self { links, order }
	}

	/// Chaitin–Briggs graph coloring with optimistic spilling.
	///
	/// `k` is the number of physical registers available (pass
	/// `RegisterConfig::num_colors()`). On success every node gets a colour
	/// in `0..k`.
	///
	/// Colors are handed out **lowest-index-first**, so a function that
	/// only needs a few registers uses only the first few — the interference
	/// graph is colored with close to the minimum number of registers
	/// rather than smearing values across the whole file. `Coloring::colors_used`
	/// reports how many distinct registers the result actually touches.
	///
	/// `Err(spills)` means some values could not be colored. There is
	/// nowhere to spill them to yet (no stack), so the caller treats this
	/// as a hard error.
	pub fn color(&self, k: usize) -> Result<Coloring<'ctx>, Vec<BasicValueEnum<'ctx>>> {
		assert!(k > 0, "need at least one register to colour with");

		// --- current degree of every not-yet-removed node --------------
		let mut degree: HashMap<BasicValueEnum<'ctx>, usize> = self
			.order
			.iter()
			.map(|n| (*n, self.links.get(n).map_or(0, HashSet::len)))
			.collect();
		let mut removed: HashSet<BasicValueEnum<'ctx>> = HashSet::new();

		// --- simplify / potential-spill: tear the graph down ----------
		// Push nodes onto `stack`; the ones pushed last (lowest degree) get
		// colored first, which is what makes the optimism in Select pay off.
		let mut stack: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(self.order.len());

		while stack.len() < self.order.len() {
			// Prefer a trivially-colourable node: degree < k means a free
			// color is guaranteed no matter what its neighbors get.
			let pick = self
				.order
				.iter()
				.find(|n| !removed.contains(*n) && degree[*n] < k)
				.copied()
				.or_else(|| {
					// Everyone left has degree >= k. Optimistically pull out
					// the most-constrained node and hope Select still finds
					// it a color once its neighbors are placed.
					self.order
						.iter()
						.filter(|n| !removed.contains(*n))
						.max_by_key(|n| degree[*n])
						.copied()
				})
				.expect("nodes remain while the stack isn't full");

			removed.insert(pick);
			stack.push(pick);

			if let Some(neighbours) = self.links.get(&pick) {
				for nbr in neighbours {
					if !removed.contains(nbr) {
						*degree.get_mut(nbr).unwrap() -= 1;
					}
				}
			}
		}

		// --- select: pop the stack, give each node its lowest free colour
		let mut colors: HashMap<BasicValueEnum<'ctx>, usize> = HashMap::new();
		let mut spills: Vec<BasicValueEnum<'ctx>> = Vec::new();

		while let Some(node) = stack.pop() {
			let mut taken = vec![false; k];
			if let Some(neighbours) = self.links.get(&node) {
				for nbr in neighbours {
					if let Some(&c) = colors.get(nbr) {
						taken[c] = true;
					}
				}
			}

			match taken.iter().position(|used| !used) {
				Some(c) => {
					colors.insert(node, c);
				}
				None => spills.push(node),
			}
		}

		if !spills.is_empty() {
			return Err(spills);
		}

		let colors_used = colors.values().copied().max().map_or(0, |m| m + 1);
		Ok(Coloring { colors, colors_used })
	}
}

pub struct Coloring<'ctx> {
	/// SSA value -> colour index in `0..k`.
	pub colors: HashMap<BasicValueEnum<'ctx>, usize>,
	/// Number of distinct colors (registers) the assignment actually uses.
	pub colors_used: usize,
}

impl<'ctx> Coloring<'ctx> {
	/// Physical register name for a value, resolving the color index
	/// through the allocatable pool (`target.allocatable()`, in file order).
	pub fn register_for(&self, value: BasicValueEnum<'ctx>, target: &RegisterConfig) -> Option<String> {
		let index = *self.colors.get(&value)?;
		target.allocatable().nth(index).map(|r| r.name.clone())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::compiler::register_allocation::liveness::compute_liveness;
	use inkwell::context::Context;

	/// Every edge in the interference graph must join two differently
	/// colored nodes.
	fn assert_valid(graph: &Graph, coloring: &Coloring) {
		for (node, neighbours) in &graph.links {
			for nbr in neighbours {
				assert_ne!(
					coloring.colors.get(node),
					coloring.colors.get(nbr),
					"interfering values got the same colour"
				);
			}
		}
	}

	#[test]
	fn colors_a_straight_line_function() {
		let ctx = Context::create();
		let module = ctx.create_module("t");
		let i32t = ctx.i32_type();
		let f = module.add_function("f", i32t.fn_type(&[i32t.into(), i32t.into()], false), None);
		let bb = ctx.append_basic_block(f, "entry");
		let b = ctx.create_builder();
		b.position_at_end(bb);

		let a = f.get_nth_param(0).unwrap().into_int_value();
		let c = f.get_nth_param(1).unwrap().into_int_value();
		let sum = b.build_int_add(a, c, "sum").unwrap();
		let inc = b.build_int_add(sum, i32t.const_int(1, false), "inc").unwrap();
		b.build_return(Some(&inc)).unwrap();

		let liveness = compute_liveness(f);
		let graph = Graph::build_interference_graph(f, &liveness);

		let coloring = graph.color(8).expect("plenty of registers");
		assert_valid(&graph, &coloring);
		// a and the two params overlap at most pairwise -> 2 colours is enough
		assert!(coloring.colors_used <= 2, "used {}", coloring.colors_used);
	}

	#[test]
	fn colouring_is_deterministic() {
		let ctx = Context::create();
		let module = ctx.create_module("t");
		let i32t = ctx.i32_type();
		let f = module.add_function("f", i32t.fn_type(&[i32t.into(), i32t.into()], false), None);
		let bb = ctx.append_basic_block(f, "entry");
		let b = ctx.create_builder();
		b.position_at_end(bb);
		let a = f.get_nth_param(0).unwrap().into_int_value();
		let c = f.get_nth_param(1).unwrap().into_int_value();
		let x = b.build_int_add(a, c, "x").unwrap();
		let y = b.build_int_mul(x, a, "y").unwrap();
		let z = b.build_int_sub(y, c, "z").unwrap();
		b.build_return(Some(&z)).unwrap();

		let liveness = compute_liveness(f);
		let graph = Graph::build_interference_graph(f, &liveness);

		let first = graph.color(4).unwrap();
		for _ in 0..20 {
			let again = graph.color(4).unwrap();
			assert_eq!(first.colors, again.colors);
		}
	}

	#[test]
	fn reports_spills_when_out_of_registers() {
		let ctx = Context::create();
		let module = ctx.create_module("t");
		let i32t = ctx.i32_type();
		let f = module.add_function("f", i32t.fn_type(&[i32t.into(), i32t.into()], false), None);
		let bb = ctx.append_basic_block(f, "entry");
		let b = ctx.create_builder();
		b.position_at_end(bb);

		let a = f.get_nth_param(0).unwrap().into_int_value();
		let c = f.get_nth_param(1).unwrap().into_int_value();

		// Build several values that are all live simultaneously (each is used
		// in the final sum), forcing more than `k` to overlap.
		let mut vals = vec![b.build_int_add(a, c, "v0").unwrap()];
		for i in 1..6 {
			let v = b.build_int_add(vals[i - 1], a, &format!("v{i}")).unwrap();
			vals.push(v);
		}
		let mut acc = vals[0];
		for v in &vals[1..] {
			acc = b.build_int_add(acc, *v, "acc").unwrap();
		}
		b.build_return(Some(&acc)).unwrap();

		let liveness = compute_liveness(f);
		let graph = Graph::build_interference_graph(f, &liveness);

		assert!(graph.color(2).is_err(), "2 registers cannot be enough here");
	}
}
