use inkwell::module::Module;
use inkwell::values::InstructionValue;
use std::collections::HashMap;

struct RegAlloc<'ctx> {
	next: usize,                                          // Index in to t0...t6 pool
	assigned: HashMap<InstructionValue<'ctx>, String>,    // LLVM value -> register name
}

impl<'ctx> RegAlloc<'ctx> {
	fn new() -> Self {
		RegAlloc { next: 0, assigned: HashMap::new() }
	}

	fn get_or_assign(&mut self, val: InstructionValue<'ctx>) -> String {
		if let Some(reg) = self.assigned.get(&val) {
			return reg.clone();
		}

		let reg = format!("t{}", self.next);
		self.next += 1;
		self.assigned.insert(val, reg.clone());
		reg
	}
}

pub fn build(module: &Module){
	
}