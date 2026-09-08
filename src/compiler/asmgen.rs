use crate::compiler::codegen::{IO_IN_FN, IO_OUT_FN};
use crate::compiler::register_allocation::config::RegisterConfig;
use crate::compiler::register_allocation::{self, Coloring};
use crate::cpu::RAM_SIZE;
use inkwell::basic_block::BasicBlock;
use inkwell::module::Module;
use inkwell::values::{AnyValue, AsValueRef, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue};
use inkwell::IntPredicate;
use std::collections::{HashMap, HashSet};

const IMMEDIATE_MAX_VALUE_14: i64 = 0x1FFF; // 13 bits

/// Maps SSA values to concrete physical registers for one function.
///
/// The graph-colouring allocator (`register_allocation`) decides where every
/// instruction result lives; this just resolves lookups and hands out the
/// codegen scratch registers for things the allocator never sees (constants,
/// parallel-copy cycle breaks).
struct RegAlloc<'ctx> {
	/// SSA value -> register name. Seeded from the colouring, plus the
	/// parameter bindings (a0, a1, ...).
	assigned: HashMap<BasicValueEnum<'ctx>, String>,
	/// Scratch register names from the target config, e.g. t4/t5/t6.
	scratch: Vec<String>,
	/// How many scratch registers are live for the instruction currently
	/// being lowered. Reset via `scratch_reset` before each one.
	scratch_in_use: usize,
	/// `i32`/`u32` SSA value -> RAM address of its low word (high word is
	/// `+1`). Wide values don't get registers; they live in a block of RAM
	/// scratch slots. See [`i32bit`].
	i32_slots: HashMap<BasicValueEnum<'ctx>, u16>,
	/// Next free wide slot's low-word address (slots grow downward).
	i32_next_slot: u16,
}

impl<'ctx> RegAlloc<'ctx> {
	fn new(coloring: &Coloring<'ctx>, config: &RegisterConfig) -> Self {
		let assigned = coloring
			.colors
			.keys()
			.map(|&value| {
				let reg = coloring
					.register_for(value, config)
					.expect("coloured value maps to an allocatable register");
				(value, reg)
			})
			.collect();

		let scratch = config.scratch_registers().map(|r| r.name.clone()).collect();

		RegAlloc {
			assigned,
			scratch,
			scratch_in_use: 0,
			i32_slots: HashMap::new(),
			i32_next_slot: i32bit::SLOT_TOP,
		}
	}

	/// RAM low-word address for a wide (`i32`) SSA value, assigning a fresh
	/// slot the first time it's seen.
	fn i32_slot(&mut self, val: BasicValueEnum<'ctx>) -> u16 {
		if let Some(&a) = self.i32_slots.get(&val) {
			return a;
		}
		let a = self.i32_next_slot;
		assert!(
			a >= i32bit::SLOT_FLOOR,
			"ran out of i32 scratch slots (RAM {}..{} is full) - too many live 32-bit \
			 temporaries in one function",
			i32bit::SLOT_FLOOR, i32bit::SLOT_TOP + 1,
		);
		self.i32_next_slot -= 2;
		self.i32_slots.insert(val, a);
		a
	}

	/// Pin a value to a specific register outside the colouring - used for
	/// parameters, which live in their argument registers.
	fn bind(&mut self, val: BasicValueEnum<'ctx>, reg: &str) {
		self.assigned.insert(val, reg.to_string());
	}

	/// Register holding `val`. Panics if it was never assigned one, which
	/// means the allocator and instruction selection disagree about what
	/// needs a register.
	fn resolve(&self, val: BasicValueEnum<'ctx>) -> String {
		self.assigned.get(&val).cloned().unwrap_or_else(|| {
			panic!("value has no register (not coloured, not a bound parameter)\n{}", val)
		})
	}

	/// Forget any scratch registers handed out for the previous instruction.
	fn scratch_reset(&mut self) {
		self.scratch_in_use = 0;
	}

	/// Hand out the next free scratch register for the instruction being
	/// lowered. Valid only until the next `scratch_reset`.
	fn next_scratch(&mut self) -> String {
		let reg = self.scratch.get(self.scratch_in_use).unwrap_or_else(|| {
			panic!(
				"code generator ran out of scratch registers (only {} configured)",
				self.scratch.len()
			)
		});
		self.scratch_in_use += 1;
		reg.clone()
	}
}

fn apply_immediate_operation<'ctx>(
	registers: &mut RegAlloc<'ctx>,
	instruction: InstructionValue<'ctx>,
	op_name: &str) -> String {
	let lhs = instruction.get_operand(0).unwrap().unwrap_value();
	let rhs = instruction.get_operand(1).unwrap().unwrap_value();

	let mut out = String::new();

	let lhs_reg = resolve_operand(registers, &mut out, lhs);
	let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

	if let BasicValueEnum::IntValue(rhs_iv) = rhs {
		if rhs_iv.is_const() {
			if let Some(imm) = rhs_iv.get_sign_extended_constant() {
				if op_name == "sl" || op_name == "sr" || op_name == "sra" {
					if imm >= 16 {
						eprintln!("warning: shift amount {imm} exceeds 4-bit shifter width, will truncate to {}", imm & 0xF)
					} else if imm < 0 {
						eprintln!("warning: shift amount {imm} is less than 0, will truncate to {}", imm & 0xF)
					}
				}
				if imm >= -(IMMEDIATE_MAX_VALUE_14 + 1) && imm <= IMMEDIATE_MAX_VALUE_14 {
					out.push_str(&format!("\t{op_name}i\t{lhs_reg} {imm} {dest}\n"));
					return out;
				}
			}
		}
	}

	let rhs_reg = resolve_operand(registers, &mut out, rhs);
	out.push_str(&format!("\t{op_name}\t{lhs_reg} {rhs_reg} {dest}\n"));

	out
}

/// Emit the test for a conditional `br` as a single branch to `target`.
///
/// `taken_on_then` picks which edge the branch selects: `true` means the
/// branch is taken exactly when the THEN edge should be followed, `false`
/// means it is taken when the ELSE edge should be followed (the predicate is
/// inverted for that case).
fn emit_cond_branch<'ctx>(
	registers: &mut RegAlloc<'ctx>,
	out: &mut String,
	fused: Option<InstructionValue<'ctx>>,
	cond: BasicValueEnum<'ctx>,
	target: &str,
	taken_on_then: bool,
) {
	if let Some(cond_instr) = fused {
		// A fused `icmp` on 32-bit operands needs the two-word compare.
		if i32bit::involves_i32(cond_instr) {
			let predicate = cond_instr.get_icmp_predicate().unwrap();
			i32bit::emit_i32_cmp_branch(out, cond_instr, registers, predicate, target, taken_on_then);
			return;
		}
		let lhs = cond_instr.get_operand(0).unwrap().unwrap_value();
		let rhs = cond_instr.get_operand(1).unwrap().unwrap_value();
		let lhs_reg = resolve_operand(registers, out, lhs);
		let rhs_reg = resolve_operand(registers, out, rhs);
		let predicate = cond_instr.get_icmp_predicate().unwrap();
		let predicate = if taken_on_then { predicate } else { invert_predicate(predicate) };
		out.push_str(&format!("\t{}\t{lhs_reg} {rhs_reg} {target}\n", branch_mnemonic(predicate)));
	} else {
		// The condition is a materialised 0/1 value: take the branch when it
		// is non-zero (THEN) or zero (ELSE).
		let cond_reg = resolve_operand(registers, out, cond);
		let mnemonic = if taken_on_then { "bne" } else { "beq" };
		out.push_str(&format!("\t{mnemonic}\t{cond_reg} zero {target}\n"));
	}
}

fn match_opcode<'ctx>(
	instruction: InstructionValue<'ctx>,
	registers: &mut RegAlloc<'ctx>,
	fn_name: &str,
	next_block_name: Option<&str>,
	is_main: bool,
) -> String {
	// Anything touching a 32-bit value is lowered word-at-a-time against RAM
	// scratch slots, not registers - see `mod i32bit`.
	if i32bit::involves_i32(instruction) {
		return i32bit::lower(instruction, registers, fn_name);
	}

	match instruction.get_opcode() {
		InstructionOpcode::Add => {
			apply_immediate_operation(registers, instruction, "add")
		}

		InstructionOpcode::Sub => {
			apply_immediate_operation(registers, instruction, "sub")
		}

		InstructionOpcode::Xor => {
			apply_immediate_operation(registers, instruction, "xor")
		}

		InstructionOpcode::Or => {
			apply_immediate_operation(registers, instruction, "or")
		}

		InstructionOpcode::And => {
			apply_immediate_operation(registers, instruction, "and")
		}

		InstructionOpcode::Shl => {
			apply_immediate_operation(registers, instruction, "sl")
		}

		InstructionOpcode::LShr => {
			apply_immediate_operation(registers, instruction, "sr")
		}

		InstructionOpcode::AShr => {
			apply_immediate_operation(registers, instruction, "sra")
		}

		InstructionOpcode::Load => {
			let addr = instruction.get_operand(0).unwrap().unwrap_value();


			let mut out = String::new();
			let (base_reg, offset) = resolve_address(registers, &mut out, addr);

			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());
			out.push_str(&format!("\tlod\t{dest} {offset}({base_reg})\n"));
			out
		}

		InstructionOpcode::Store => {
			let value = instruction.get_operand(0).unwrap().unwrap_value();
			let addr = instruction.get_operand(1).unwrap().unwrap_value();

			let mut out = String::new();
			let value_reg = resolve_operand(registers, &mut out, value);
			let (base_reg, offset) = resolve_address(registers, &mut out, addr);

			out.push_str(&format!("\tstr\t{value_reg} {offset}({base_reg})\n"));
			out
		}

		InstructionOpcode::ICmp => {
			if is_fused_into_next_br(instruction) {
				String::new() // handled by Br below, same as today
			} else {
				materialize_icmp(instruction, registers)
			}
		}

		InstructionOpcode::Br => {
			let mut out = String::new();
			let pred = instruction.get_parent().unwrap();

			// --- unconditional branch ------------------------------------
			if instruction.get_num_operands() == 1 {
				let target = instruction.get_operand(0).unwrap().unwrap_block();
				let target_name = target.get_name().to_str().unwrap().to_string();
				out.push_str(&phi_moves_for_edge(pred, target, registers));
				if next_block_name != Some(target_name.as_str()) {
					out.push_str(&format!("\tj\t{fn_name}_{target_name}\n"));
				}
				return out;
			}

			// --- conditional branch ------------------------------------
			let cond = instruction.get_operand(0).unwrap().unwrap_value();
			let then_bb = instruction.get_operand(2).unwrap().unwrap_block();
			let else_bb = instruction.get_operand(1).unwrap().unwrap_block();
			let then_name = then_bb.get_name().to_str().unwrap().to_string();
			let else_name = else_bb.get_name().to_str().unwrap().to_string();

			// Phi copies belong on their edge. Build them up front so we know
			// whether we can use the tight fall-through form or need a
			// per-edge trampoline.
			let then_moves = phi_moves_for_edge(pred, then_bb, registers);
			let else_moves = phi_moves_for_edge(pred, else_bb, registers);
			registers.scratch_reset();

			// Is the condition an icmp sitting right before us? Then we test
			// its operands directly instead of a materialised 0/1 value.
			let fused = match cond {
				BasicValueEnum::IntValue(iv) => iv.as_instruction().filter(|ci| {
					ci.get_opcode() == InstructionOpcode::ICmp
						&& instruction.get_previous_instruction() == Some(*ci)
				}),
				_ => None,
			};

			let then_label = format!("{fn_name}_{then_name}");
			let else_label = format!("{fn_name}_{else_name}");

			// No phi copies on either edge: a single conditional branch plus at
			// most one fall-through jump.
			if then_moves.is_empty() && else_moves.is_empty() {
				if next_block_name == Some(then_name.as_str()) {
					emit_cond_branch(registers, &mut out, fused, cond, &else_label, false);
				} else {
					emit_cond_branch(registers, &mut out, fused, cond, &then_label, true);
					if next_block_name != Some(else_name.as_str()) {
						out.push_str(&format!("\tj\t{else_label}\n"));
					}
				}
				return out;
			}

			// Only one edge carries copies: branch straight to the *other*
			// block, then fall through this edge's copies into its block. The
			// copies sit after the branch, so they run only when this edge is
			// taken - and can't clobber the branch operands.
			if else_moves.is_empty() {
				emit_cond_branch(registers, &mut out, fused, cond, &else_label, false);
				out.push_str(&then_moves);
				if next_block_name != Some(then_name.as_str()) {
					out.push_str(&format!("\tj\t{then_label}\n"));
				}
				return out;
			}
			if then_moves.is_empty() {
				emit_cond_branch(registers, &mut out, fused, cond, &then_label, true);
				out.push_str(&else_moves);
				if next_block_name != Some(else_name.as_str()) {
					out.push_str(&format!("\tj\t{else_label}\n"));
				}
				return out;
			}

			// Both edges carry copies. Emit a trampoline so each edge's copies
			// run only when that edge is taken:
			//
			//     <test>  branch to L_then when the THEN condition holds
			//     <else edge copies>
			//     j else
			//   L_then:
			//     <then edge copies>
			//     j then          (dropped when `then` is the next block)
			let id = instruction.as_value_ref() as usize;
			let trampoline = format!("{fn_name}_br{id}_then");
			emit_cond_branch(registers, &mut out, fused, cond, &trampoline, true);
			out.push_str(&else_moves);
			out.push_str(&format!("\tj\t{else_label}\n"));
			out.push_str(&format!("{trampoline}:\n"));
			out.push_str(&then_moves);
			if next_block_name != Some(then_name.as_str()) {
				out.push_str(&format!("\tj\t{then_label}\n"));
			}
			out
		}

		InstructionOpcode::Return => {
			let mut out = String::new();
			if let Some(val) = instruction.get_operand(0) {
				let val_reg = resolve_operand(registers, &mut out, val.unwrap_value());
				out.push_str(&format!("\tmv\t{val_reg} a0\n"));
			}
			// main returning isn't "return to a caller" - there is no caller.
			// It's the program ending.
			out.push_str(if is_main { "\thlt\n" } else { "\tret\n" });

			out
		}

		InstructionOpcode::ZExt => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

			// Both source and destination fit in a single 16-bit word, so this
			// is just a copy into the destination register the allocator chose
			// (and folds away entirely when they coincide - see the self-move
			// peephole). NOTE: this assumes dest fits in ≤16 bits. Zero-extending
			// into a real 32-bit value (two registers, low+high) isn't handled
			// yet, that needs the ADDF/SUBF two-register i32 convention here too.
			if src_reg != dest {
				out.push_str(&format!("\tmv\t{src_reg} {dest}\n"));
			}
			out
		}

		InstructionOpcode::IntToPtr => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

			// On this hardware a pointer IS just a 16-bit address — inttoptr is a
			// pure reinterpretation, not a real operation. Copy into dest (folds
			// away when src and dest share a register).
			if src_reg != dest {
				out.push_str(&format!("\tmv\t{src_reg} {dest}\n"));
			}
			out
		}

		InstructionOpcode::PtrToInt => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());
			if src_reg != dest {
				out.push_str(&format!("\tmv\t{src_reg} {dest}\n"));
			}
			out
		}

		InstructionOpcode::Select => {
			let cond = instruction.get_operand(0).unwrap().unwrap_value();
			let true_val = instruction.get_operand(1).unwrap().unwrap_value();
			let false_val = instruction.get_operand(2).unwrap().unwrap_value();

			let mut out = String::new();
			let cond_reg = resolve_operand(registers, &mut out, cond);
			let true_reg = resolve_operand(registers, &mut out, true_val);
			let false_reg = resolve_operand(registers, &mut out, false_val);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

			// Raw pointer as a cheap unique id for label naming - same trick as
			// materialize_icmp, works regardless of the select's result type.
			let id = instruction.as_value_ref() as usize;
			let end_label = format!("select_end_{id}");

			out.push_str(&format!("\tmv\t{false_reg} {dest}\n"));
			out.push_str(&format!("\tbeq\t{cond_reg} zero {end_label}\n"));
			out.push_str(&format!("\tmv\t{true_reg} {dest}\n"));
			out.push_str(&format!("{end_label}:\n"));

			out
		}

		InstructionOpcode::Call => lower_call(instruction, registers),

		// The target has no multiply/divide unit, so these expand to
		// shift/add/subtract code right here - constants get a short shift
		// chain, variables get an unrolled bit loop. See `muldiv` below.
		InstructionOpcode::Mul => muldiv::lower_mul(instruction, registers, fn_name),
		InstructionOpcode::UDiv => muldiv::lower_div_rem(instruction, registers, fn_name, muldiv::DivKind::UDiv),
		InstructionOpcode::SDiv => muldiv::lower_div_rem(instruction, registers, fn_name, muldiv::DivKind::SDiv),
		InstructionOpcode::URem => muldiv::lower_div_rem(instruction, registers, fn_name, muldiv::DivKind::URem),
		InstructionOpcode::SRem => muldiv::lower_div_rem(instruction, registers, fn_name, muldiv::DivKind::SRem),

		other => panic!("instruction not selected yet: {:?}", other),
	}
}

/// The only calls that reach instruction selection are the IO-port
/// intrinsics (`__io_out` / `__io_in`, emitted by codegen for the
/// `port_out` / `port_in` builtins). Everything else is inlined away by
/// `Oz` before we get here - a surviving user call would need real
/// call-lowering (arg marshalling, caller-saves, a stack frame), none of
/// which exists yet.
fn lower_call<'ctx>(instruction: InstructionValue<'ctx>, registers: &mut RegAlloc<'ctx>) -> String {
	// For a call, operands are [args.., callee]; the callee is last.
	let num_operands = instruction.get_num_operands();
	let callee = instruction
		.get_operand(num_operands - 1)
		.unwrap()
		.unwrap_value();
	let callee_name = callee
		.into_pointer_value()
		.get_name()
		.to_str()
		.unwrap()
		.to_string();

	/// Read a call operand that must be a constant - the port number.
	fn const_port(instruction: InstructionValue, index: u32) -> i64 {
		let port = instruction
			.get_operand(index)
			.unwrap()
			.unwrap_value()
			.into_int_value()
			.get_sign_extended_constant()
			.expect("IO port operand is not a constant - codegen should have rejected this");
		assert!((0..=15).contains(&port), "IO port {port} out of range 0..=15");
		port
	}

	let mut out = String::new();

	match callee_name.as_str() {
		IO_OUT_FN => {
			// out(port, value): `out <value reg> <port>`
			let port = const_port(instruction, 0);
			let value = instruction.get_operand(1).unwrap().unwrap_value();
			let value_reg = resolve_operand(registers, &mut out, value);
			out.push_str(&format!("\tout\t{value_reg} {port}\n"));
		}
		IO_IN_FN => {
			// in(port) -> i16: `in <dest reg> <port>`
			let port = const_port(instruction, 0);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());
			out.push_str(&format!("\tin\t{dest} {port}\n"));
		}
		other => panic!(
			"call to `{other}` reached instruction selection - only the IO-port \
			 intrinsics are supported; user calls aren't lowered yet"
		),
	}

	out
}


// =============================================================================
// 32-bit (`i32` / `u32`) support
// =============================================================================
//
// The machine is 16-bit: 16-bit registers, a 16-bit ALU (with `addf`/`subf`
// carry-chain ops), a 4-bit shifter. A 32-bit value is therefore kept as two
// 16-bit words in a pair of RAM scratch slots - it never touches a register
// except transiently while an op runs. The register allocator is told to skip
// wide values entirely (`is_register_value`), so nothing here fights it.
//
// Slots grow downward from just under the stack pointer; a small fixed block
// below them is working storage for the multiply/divide loops.
//
//     RAM: [ .. program/heap .. ][ WORK 8w ][ .. i32 slots .. ][ sp ]
//                                 168..175   176 up to 253      255
//
// `-Oz` narrows i32 to i16 wherever the top half is provably dead, so what
// actually reaches here is the arithmetic that genuinely needs 32 bits:
// fixed-point multiplies, wide divides, loop-carried 32-bit accumulators.
mod i32bit {
	use super::{as_constant_i64, branch_mnemonic, invert_predicate, RegAlloc};
	use inkwell::values::{AnyValue, AsValueRef, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue};
	use inkwell::{types::AnyTypeEnum, IntPredicate};

	/// Low-word address of the first (highest) wide slot. `sp` is `RAM-1`,
	/// slot hi-words sit at this + 1, so this leaves `sp` alone.
	pub const SLOT_TOP: u16 = (crate::cpu::RAM_SIZE as u16) - 3;
	/// Slots may descend to here (≈38 of them). Below this is the work block.
	pub const SLOT_FLOOR: u16 = 176;
	/// 8 words of fixed scratch for the multiply / divide loops.
	const WORK: u16 = 168;

	/// Exactly `i32`/`u32`. Wider ints (`i64`) only ever appear here from
	/// LLVM's own pointer-index arithmetic, which is address-sized in
	/// practice - those keep the legacy "just use the low 16 bits" path.
	fn ty_is_wide(t: AnyTypeEnum) -> bool {
		matches!(t, AnyTypeEnum::IntType(it) if it.get_bit_width() == 32)
	}

	fn val_is_wide(v: BasicValueEnum) -> bool {
		matches!(v, BasicValueEnum::IntValue(iv) if iv.get_type().get_bit_width() == 32)
	}

	fn operand(instr: InstructionValue, i: u32) -> BasicValueEnum {
		instr.get_operand(i).unwrap().unwrap_value()
	}

	/// Does this instruction produce or consume a 32-bit value?
	pub fn involves_i32(instr: InstructionValue) -> bool {
		if ty_is_wide(instr.get_type()) {
			return true;
		}
		match instr.get_opcode() {
			// narrow result, wide input
			InstructionOpcode::Trunc | InstructionOpcode::ICmp | InstructionOpcode::Store => {
				val_is_wide(operand(instr, 0))
			}
			_ => false,
		}
	}

	/// Where a wide operand's two words come from.
	enum Wide {
		Const(u16, u16),
		Slot(u16),
	}

	fn wide_src<'ctx>(v: BasicValueEnum<'ctx>, regs: &mut RegAlloc<'ctx>) -> Wide {
		if let Some(c) = as_constant_i64(v) {
			let u = c as i64 as u64;
			Wide::Const((u & 0xFFFF) as u16, ((u >> 16) & 0xFFFF) as u16)
		} else {
			Wide::Slot(regs.i32_slot(v))
		}
	}

	/// Emit: load word `k` (0 = low, 1 = high) of `src` into register `reg`.
	fn load_word(out: &mut String, reg: &str, src: &Wide, k: u16) {
		match src {
			Wide::Const(lo, hi) => {
				let w = if k == 0 { *lo } else { *hi };
				out.push_str(&format!("\tli\t{} {reg}\n", w as i16));
			}
			Wide::Slot(a) => out.push_str(&format!("\tlod\t{reg} {}(zero)\n", a + k)),
		}
	}

	fn store_word(out: &mut String, reg: &str, slot: u16, k: u16) {
		out.push_str(&format!("\tstr\t{reg} {}(zero)\n", slot + k));
	}

	/// Register holding a narrow (`i16`) SSA value or a constant.
	fn narrow_reg<'ctx>(out: &mut String, v: BasicValueEnum<'ctx>, regs: &mut RegAlloc<'ctx>) -> String {
		super::resolve_operand(regs, out, v)
	}

	pub fn lower<'ctx>(
		instr: InstructionValue<'ctx>,
		regs: &mut RegAlloc<'ctx>,
		fn_name: &str,
	) -> String {
		let mut out = String::new();
		let op = instr.get_opcode();

		match op {
			InstructionOpcode::Add | InstructionOpcode::Sub => {
				let (lo_op, hi_op) = if op == InstructionOpcode::Add {
					("add", "addf")
				} else {
					("sub", "subf")
				};
				let a = wide_src(operand(instr, 0), regs);
				let b = wide_src(operand(instr, 1), regs);
				let d = regs.i32_slot(wide_self(instr));
				// low word sets carry/borrow; nothing between here and the
				// high word may touch flags (lod/str/li don't).
				load_word(&mut out, "t4", &a, 0);
				load_word(&mut out, "t5", &b, 0);
				out.push_str(&format!("\t{lo_op}\tt4 t5 t4\n"));
				store_word(&mut out, "t4", d, 0);
				load_word(&mut out, "t4", &a, 1);
				load_word(&mut out, "t5", &b, 1);
				out.push_str(&format!("\t{hi_op}\tt4 t5 t4\n"));
				store_word(&mut out, "t4", d, 1);
			}

			InstructionOpcode::And | InstructionOpcode::Or | InstructionOpcode::Xor => {
				let m = match op {
					InstructionOpcode::And => "and",
					InstructionOpcode::Or => "or",
					_ => "xor",
				};
				let a = wide_src(operand(instr, 0), regs);
				let b = wide_src(operand(instr, 1), regs);
				let d = regs.i32_slot(wide_self(instr));
				for k in 0..2 {
					load_word(&mut out, "t4", &a, k);
					load_word(&mut out, "t5", &b, k);
					out.push_str(&format!("\t{m}\tt4 t5 t4\n"));
					store_word(&mut out, "t4", d, k);
				}
			}

			InstructionOpcode::Shl | InstructionOpcode::LShr | InstructionOpcode::AShr => {
				lower_shift(&mut out, instr, regs, fn_name);
			}

			InstructionOpcode::Mul => lower_mul32(&mut out, instr, regs, fn_name),

			InstructionOpcode::UDiv
			| InstructionOpcode::SDiv
			| InstructionOpcode::URem
			| InstructionOpcode::SRem => lower_divrem32(&mut out, instr, regs, fn_name),

			InstructionOpcode::ZExt => {
				let src = operand(instr, 0);
				let d = regs.i32_slot(wide_self(instr));
				let r = narrow_reg(&mut out, src, regs);
				store_word(&mut out, &r, d, 0);
				out.push_str("\tli\t0 t4\n");
				store_word(&mut out, "t4", d, 1);
			}

			InstructionOpcode::SExt => {
				let src = operand(instr, 0);
				let d = regs.i32_slot(wide_self(instr));
				let r = narrow_reg(&mut out, src, regs);
				store_word(&mut out, &r, d, 0);
				// high word = 0 or -1 depending on sign of the low word
				out.push_str(&format!("\tsrai\t{r} 15 t4\n"));
				store_word(&mut out, "t4", d, 1);
			}

			InstructionOpcode::Trunc => {
				let src = wide_src(operand(instr, 0), regs);
				let dest = regs.resolve(instr.as_any_value_enum().try_into().unwrap());
				load_word(&mut out, &dest, &src, 0);
			}

			InstructionOpcode::ICmp => {
				let dest = regs.resolve(instr.as_any_value_enum().try_into().unwrap());
				let pred = instr.get_icmp_predicate().unwrap();
				let id = instr.as_value_ref() as usize;
				let t = format!("{fn_name}_i32cmp{id}_t");
				let e = format!("{fn_name}_i32cmp{id}_e");
				out.push_str(&format!("\tli\t0 {dest}\n"));
				emit_i32_cmp_branch(&mut out, instr, regs, pred, &t, true);
				out.push_str(&format!("\tj\t{e}\n"));
				out.push_str(&format!("{t}:\n"));
				out.push_str(&format!("\tli\t1 {dest}\n"));
				out.push_str(&format!("{e}:\n"));
			}

			InstructionOpcode::Select => {
				let cond = operand(instr, 0);
				let tval = wide_src(operand(instr, 1), regs);
				let fval = wide_src(operand(instr, 2), regs);
				let d = regs.i32_slot(wide_self(instr));
				let cond_reg = narrow_reg(&mut out, cond, regs);
				let id = instr.as_value_ref() as usize;
				let end = format!("{fn_name}_i32sel{id}_end");
				for k in 0..2 {
					load_word(&mut out, "t4", &fval, k);
					store_word(&mut out, "t4", d, k);
				}
				out.push_str(&format!("\tbeqz\t{cond_reg} {end}\n"));
				for k in 0..2 {
					load_word(&mut out, "t4", &tval, k);
					store_word(&mut out, "t4", d, k);
				}
				out.push_str(&format!("{end}:\n"));
			}

			InstructionOpcode::Load => {
				let addr = operand(instr, 0);
				let d = regs.i32_slot(wide_self(instr));
				let (base, off) = super::resolve_address(regs, &mut out, addr);
				out.push_str(&format!("\tlod\tt4 {off}({base})\n"));
				store_word(&mut out, "t4", d, 0);
				out.push_str(&format!("\tlod\tt4 {}({base})\n", off + 1));
				store_word(&mut out, "t4", d, 1);
			}

			InstructionOpcode::Store => {
				let value = wide_src(operand(instr, 0), regs);
				let addr = operand(instr, 1);
				let (base, off) = super::resolve_address(regs, &mut out, addr);
				load_word(&mut out, "t4", &value, 0);
				out.push_str(&format!("\tstr\tt4 {off}({base})\n"));
				load_word(&mut out, "t4", &value, 1);
				out.push_str(&format!("\tstr\tt4 {}({base})\n", off + 1));
			}

			InstructionOpcode::PtrToInt => {
				let src = operand(instr, 0);
				let d = regs.i32_slot(wide_self(instr));
				let r = super::resolve_operand(regs, &mut out, src);
				store_word(&mut out, &r, d, 0);
				out.push_str("\tli\t0 t4\n");
				store_word(&mut out, "t4", d, 1);
			}

			InstructionOpcode::IntToPtr => {
				let src = wide_src(operand(instr, 0), regs);
				let dest = regs.resolve(instr.as_any_value_enum().try_into().unwrap());
				load_word(&mut out, &dest, &src, 0);
			}

			InstructionOpcode::Call => lower_wide_intrinsic(&mut out, instr, regs, fn_name),

			other => panic!("32-bit `{other:?}` is not lowered yet"),
		}

		out
	}

	fn wide_self(instr: InstructionValue) -> BasicValueEnum {
		instr.as_any_value_enum().try_into().unwrap()
	}

	/// Parallel copy of the wide phis on one edge: `(dest slot, incoming)`.
	/// Sources are staged through the WORK block first so a swap
	/// (`a <- b, b <- a`) is still correct.
	pub fn emit_phi_copies<'ctx>(
		pairs: &[(u16, BasicValueEnum<'ctx>)],
		regs: &mut RegAlloc<'ctx>,
	) -> String {
		let mut out = String::new();
		if pairs.is_empty() {
			return out;
		}
		assert!(
			pairs.len() * 2 <= (SLOT_FLOOR - WORK) as usize,
			"more simultaneous 32-bit phi copies on one edge than the staging area holds"
		);
		for (j, (_dst, src)) in pairs.iter().enumerate() {
			let s = wide_src(*src, regs);
			let stage = WORK + (j as u16) * 2;
			load_word(&mut out, "t4", &s, 0);
			store_word(&mut out, "t4", stage, 0);
			load_word(&mut out, "t4", &s, 1);
			store_word(&mut out, "t4", stage, 1);
		}
		for (j, (dst, _src)) in pairs.iter().enumerate() {
			let stage = WORK + (j as u16) * 2;
			out.push_str(&format!("\tlod\tt4 {}(zero)\n", stage));
			store_word(&mut out, "t4", *dst, 0);
			out.push_str(&format!("\tlod\tt4 {}(zero)\n", stage + 1));
			store_word(&mut out, "t4", *dst, 1);
		}
		out
	}

	// --- shifts --------------------------------------------------------------

	fn lower_shift<'ctx>(
		out: &mut String,
		instr: InstructionValue<'ctx>,
		regs: &mut RegAlloc<'ctx>,
		fn_name: &str,
	) {
		let x = wide_src(operand(instr, 0), regs);
		let d = regs.i32_slot(wide_self(instr));
		let op = instr.get_opcode();
		let arith = op == InstructionOpcode::AShr;
		let right = arith || op == InstructionOpcode::LShr;

		let amount = as_constant_i64(operand(instr, 1))
			.unwrap_or_else(|| panic!("{fn_name}: 32-bit shift by a non-constant amount isn't supported yet - lift the shift amount to a `const`"));
		let s = amount.rem_euclid(32) as u16;

		// x lives in t4 (lo) / t5 (hi); result written back from the same regs
		load_word(out, "t4", &x, 0);
		load_word(out, "t5", &x, 1);

		if s == 0 {
			// no-op
		} else if !right {
			// shift left
			if s < 16 {
				// hi = (hi << s) | (lo >>u (16-s));  lo = lo << s
				out.push_str(&format!("\tsri\tt4 {} t6\n", 16 - s));
				out.push_str(&format!("\tsli\tt5 {s} t5\n"));
				out.push_str("\tor\tt5 t6 t5\n");
				out.push_str(&format!("\tsli\tt4 {s} t4\n"));
			} else {
				// hi = lo << (s-16); lo = 0
				out.push_str(&format!("\tsli\tt4 {} t5\n", s - 16));
				out.push_str("\tli\t0 t4\n");
			}
		} else {
			// shift right (logical or arithmetic)
			let hi_sr = if arith { "srai" } else { "sri" };
			if s < 16 {
				// lo = (lo >>u s) | (hi << (16-s));  hi = hi (>>s)
				out.push_str(&format!("\tsli\tt5 {} t6\n", 16 - s));
				out.push_str(&format!("\tsri\tt4 {s} t4\n"));
				out.push_str("\tor\tt4 t6 t4\n");
				out.push_str(&format!("\t{hi_sr}\tt5 {s} t5\n"));
			} else {
				// lo = hi (>>(s-16));  hi = sign/zero fill
				out.push_str(&format!("\t{hi_sr}\tt5 {} t4\n", s - 16));
				if arith {
					out.push_str("\tsrai\tt5 15 t5\n");
				} else {
					out.push_str("\tli\t0 t5\n");
				}
			}
		}
		store_word(out, "t4", d, 0);
		store_word(out, "t5", d, 1);
	}

	// --- comparisons -------------------------------------------------------

	/// Branch to `target` when the i32 comparison in `instr` holds
	/// (`taken_on_true`) or fails (`!taken_on_true`). Uses t4/t5/t6.
	///
	/// Ordered compares decide on the high word (signed or unsigned per the
	/// predicate); when the high words are equal they fall through to an
	/// **unsigned** compare of the low word - which is a magnitude either way.
	pub fn emit_i32_cmp_branch<'ctx>(
		out: &mut String,
		instr: InstructionValue<'ctx>,
		regs: &mut RegAlloc<'ctx>,
		pred: IntPredicate,
		target: &str,
		taken_on_true: bool,
	) {
		let pred = if taken_on_true { pred } else { invert_predicate(pred) };
		let a = wide_src(operand(instr, 0), regs);
		let b = wide_src(operand(instr, 1), regs);
		let id = instr.as_value_ref() as usize;

		load_word(out, "t4", &a, 1);
		load_word(out, "t5", &b, 1);

		match pred {
			IntPredicate::EQ => {
				let no = format!("i32cmp{id}_no");
				out.push_str(&format!("\tbne\tt4 t5 {no}\n"));
				load_word(out, "t4", &a, 0);
				load_word(out, "t5", &b, 0);
				out.push_str(&format!("\tbeq\tt4 t5 {target}\n"));
				out.push_str(&format!("{no}:\n"));
			}
			IntPredicate::NE => {
				out.push_str(&format!("\tbne\tt4 t5 {target}\n"));
				load_word(out, "t4", &a, 0);
				load_word(out, "t5", &b, 0);
				out.push_str(&format!("\tbne\tt4 t5 {target}\n"));
			}
			_ => {
				let hi = branch_mnemonic(pred); // signed for S*, unsigned for U*
				let lo = match pred {
					IntPredicate::SLT | IntPredicate::ULT => "bltu",
					IntPredicate::SLE | IntPredicate::ULE => "bleu",
					IntPredicate::SGT | IntPredicate::UGT => "bgtu",
					IntPredicate::SGE | IntPredicate::UGE => "bgeu",
					_ => unreachable!("EQ/NE handled above"),
				};
				let tie = format!("i32cmp{id}_tie");
				let done = format!("i32cmp{id}_done");
				out.push_str(&format!("\tbeq\tt4 t5 {tie}\n"));
				out.push_str(&format!("\t{hi}\tt4 t5 {target}\n"));
				out.push_str(&format!("\tj\t{done}\n"));
				out.push_str(&format!("{tie}:\n"));
				load_word(out, "t4", &a, 0);
				load_word(out, "t5", &b, 0);
				out.push_str(&format!("\t{lo}\tt4 t5 {target}\n"));
				out.push_str(&format!("{done}:\n"));
			}
		}
	}

	// --- min / max / abs intrinsics --------------------------------------

	fn lower_wide_intrinsic<'ctx>(
		out: &mut String,
		instr: InstructionValue<'ctx>,
		regs: &mut RegAlloc<'ctx>,
		fn_name: &str,
	) {
		let n = instr.get_num_operands();
		let callee = instr.get_operand(n - 1).unwrap().unwrap_value();
		let name = callee.into_pointer_value().get_name().to_str().unwrap().to_string();
		let d = regs.i32_slot(wide_self(instr));
		let id = instr.as_value_ref() as usize;

		let base = name.rsplit('.').nth(1).unwrap_or(&name); // llvm.smax.i32 -> smax
		if base == "abs" {
			let a = wide_src(operand(instr, 0), regs);
			let pos = format!("{fn_name}_abs{id}_pos");
			// copy a into d
			for k in 0..2 {
				load_word(out, "t4", &a, k);
				store_word(out, "t4", d, k);
			}
			// if d >= 0 (signed, high word) keep it, else negate the pair
			out.push_str(&format!("\tlod\tt4 {}(zero)\n", d + 1));
			out.push_str(&format!("\tbgez\tt4 {pos}\n"));
			neg_pair(out, d);
			out.push_str(&format!("{pos}:\n"));
			return;
		}

		// smax/smin/umax/umin: d = (a <pred> b) ? a : b
		let pred = match base {
			"smax" => IntPredicate::SGT,
			"smin" => IntPredicate::SLT,
			"umax" => IntPredicate::UGT,
			"umin" => IntPredicate::ULT,
			_ => panic!("{fn_name}: unsupported 32-bit intrinsic `{name}`"),
		};
		let a = wide_src(operand(instr, 0), regs);
		let b = wide_src(operand(instr, 1), regs);
		let take_a = format!("{fn_name}_mm{id}_a");
		let end = format!("{fn_name}_mm{id}_end");
		emit_i32_cmp_branch(out, instr, regs, pred, &take_a, true);
		for k in 0..2 {
			load_word(out, "t4", &b, k);
			store_word(out, "t4", d, k);
		}
		out.push_str(&format!("\tj\t{end}\n"));
		out.push_str(&format!("{take_a}:\n"));
		for k in 0..2 {
			load_word(out, "t4", &a, k);
			store_word(out, "t4", d, k);
		}
		out.push_str(&format!("{end}:\n"));
	}

	/// Two's-complement negate the 32-bit value in slot `s`: `s = 0 - s`.
	fn neg_pair(out: &mut String, s: u16) {
		out.push_str(&format!("\tlod\tt4 {}(zero)\n", s));
		out.push_str(&format!("\tlod\tt5 {}(zero)\n", s + 1));
		out.push_str("\tli\t0 t6\n");
		out.push_str("\tsub\tt6 t4 t4\n"); // 0 - lo, sets borrow
		store_word(out, "t4", s, 0);
		out.push_str("\tli\t0 t6\n");
		out.push_str("\tsubf\tt6 t5 t5\n"); // 0 - hi - borrow
		store_word(out, "t5", s, 1);
	}

	// --- 32-bit multiply --------------------------------------------------

	fn lower_mul32<'ctx>(
		out: &mut String,
		instr: InstructionValue<'ctx>,
		regs: &mut RegAlloc<'ctx>,
		fn_name: &str,
	) {
		let a = wide_src(operand(instr, 0), regs);
		let b = wide_src(operand(instr, 1), regs);
		let d = regs.i32_slot(wide_self(instr));
		let id = instr.as_value_ref() as usize;

		// WORK layout: P_LO P_HI  M_LO M_HI  Q_LO Q_HI
		let (p_lo, p_hi) = (WORK, WORK + 1);
		let (m_lo, m_hi) = (WORK + 2, WORK + 3);
		let (q_lo, q_hi) = (WORK + 4, WORK + 5);

		// P = 0 ; M = a ; Q = b
		out.push_str("\tli\t0 t4\n");
		store_word(out, "t4", p_lo, 0);
		store_word(out, "t4", p_hi, 0);
		load_word(out, "t4", &a, 0);
		store_word(out, "t4", m_lo, 0);
		load_word(out, "t4", &a, 1);
		store_word(out, "t4", m_hi, 0);
		load_word(out, "t4", &b, 0);
		store_word(out, "t4", q_lo, 0);
		load_word(out, "t4", &b, 1);
		store_word(out, "t4", q_hi, 0);

		let top = format!("{fn_name}_mul32_{id}_top");
		let done = format!("{fn_name}_mul32_{id}_done");
		let skip = format!("{fn_name}_mul32_{id}_skip");
		out.push_str(&format!("{top}:\n"));
		// exhausted? Q == 0
		out.push_str(&format!("\tlod\tt4 {q_lo}(zero)\n"));
		out.push_str(&format!("\tlod\tt5 {q_hi}(zero)\n"));
		out.push_str("\tor\tt4 t5 t6\n");
		out.push_str(&format!("\tbeqz\tt6 {done}\n"));
		// low bit of Q set? -> P += M  (32-bit)
		out.push_str("\tandi\tt4 1 t6\n");
		out.push_str(&format!("\tbeqz\tt6 {skip}\n"));
		out.push_str(&format!("\tlod\tt4 {p_lo}(zero)\n"));
		out.push_str(&format!("\tlod\tt5 {m_lo}(zero)\n"));
		out.push_str("\tadd\tt4 t5 t4\n");
		out.push_str(&format!("\tstr\tt4 {p_lo}(zero)\n"));
		out.push_str(&format!("\tlod\tt4 {p_hi}(zero)\n"));
		out.push_str(&format!("\tlod\tt5 {m_hi}(zero)\n"));
		out.push_str("\taddf\tt4 t5 t4\n");
		out.push_str(&format!("\tstr\tt4 {p_hi}(zero)\n"));
		out.push_str(&format!("{skip}:\n"));
		// M <<= 1  (32-bit)
		out.push_str(&format!("\tlod\tt4 {m_lo}(zero)\n"));
		out.push_str(&format!("\tlod\tt5 {m_hi}(zero)\n"));
		out.push_str("\tsri\tt4 15 t6\n");
		out.push_str("\tsli\tt5 1 t5\n");
		out.push_str("\tor\tt5 t6 t5\n");
		out.push_str("\tsli\tt4 1 t4\n");
		out.push_str(&format!("\tstr\tt4 {m_lo}(zero)\n"));
		out.push_str(&format!("\tstr\tt5 {m_hi}(zero)\n"));
		// Q >>= 1 (logical, 32-bit)
		out.push_str(&format!("\tlod\tt4 {q_lo}(zero)\n"));
		out.push_str(&format!("\tlod\tt5 {q_hi}(zero)\n"));
		out.push_str("\tsli\tt5 15 t6\n");
		out.push_str("\tsri\tt4 1 t4\n");
		out.push_str("\tor\tt4 t4 t6\n");
		out.push_str("\tsri\tt5 1 t5\n");
		out.push_str(&format!("\tstr\tt4 {q_lo}(zero)\n"));
		out.push_str(&format!("\tstr\tt5 {q_hi}(zero)\n"));
		out.push_str(&format!("\tj\t{top}\n"));
		out.push_str(&format!("{done}:\n"));
		// result = P
		out.push_str(&format!("\tlod\tt4 {p_lo}(zero)\n"));
		store_word(out, "t4", d, 0);
		out.push_str(&format!("\tlod\tt4 {p_hi}(zero)\n"));
		store_word(out, "t4", d, 1);
	}

	// --- 32-bit divide / remainder -------------------------------------

	fn lower_divrem32<'ctx>(
		out: &mut String,
		instr: InstructionValue<'ctx>,
		regs: &mut RegAlloc<'ctx>,
		fn_name: &str,
	) {
		let op = instr.get_opcode();
		let signed = matches!(op, InstructionOpcode::SDiv | InstructionOpcode::SRem);
		let want_rem = matches!(op, InstructionOpcode::URem | InstructionOpcode::SRem);
		let n = wide_src(operand(instr, 0), regs);
		let dv = wide_src(operand(instr, 1), regs);
		let d = regs.i32_slot(wide_self(instr));
		let id = instr.as_value_ref() as usize;

		// WORK: REM_LO REM_HI  NQ_LO NQ_HI  D_LO D_HI  (+ sign bytes reuse t-regs)
		let (rem_lo, rem_hi) = (WORK, WORK + 1);
		let (nq_lo, nq_hi) = (WORK + 2, WORK + 3);
		let (dd_lo, dd_hi) = (WORK + 4, WORK + 5);
		let sgn = WORK + 6; // 1 if the quotient must be negated
		let nsg = WORK + 7; // 1 if the dividend was negative (remainder sign)

		// NQ = n ; D = divisor
		for (src, base) in [(&n, nq_lo), (&dv, dd_lo)] {
			load_word(out, "t4", src, 0);
			store_word(out, "t4", base, 0);
			load_word(out, "t4", src, 1);
			store_word(out, "t4", base, 1);
		}

		if signed {
			let np = format!("{fn_name}_div32_{id}_np");
			let dp = format!("{fn_name}_div32_{id}_dp");
			// sgn = (n<0) xor (d<0) ; nsg = (n<0)
			out.push_str("\tli\t0 t4\n");
			out.push_str(&format!("\tstr\tt4 {sgn}(zero)\n"));
			out.push_str(&format!("\tstr\tt4 {nsg}(zero)\n"));
			out.push_str(&format!("\tlod\tt4 {nq_hi}(zero)\n"));
			out.push_str(&format!("\tbgez\tt4 {np}\n"));
			out.push_str("\tli\t1 t4\n");
			out.push_str(&format!("\tstr\tt4 {sgn}(zero)\n"));
			out.push_str(&format!("\tstr\tt4 {nsg}(zero)\n"));
			neg_pair(out, nq_lo);
			out.push_str(&format!("{np}:\n"));
			out.push_str(&format!("\tlod\tt4 {dd_hi}(zero)\n"));
			out.push_str(&format!("\tbgez\tt4 {dp}\n"));
			out.push_str(&format!("\tlod\tt4 {sgn}(zero)\n"));
			out.push_str("\txori\tt4 1 t4\n");
			out.push_str(&format!("\tstr\tt4 {sgn}(zero)\n"));
			neg_pair(out, dd_lo);
			out.push_str(&format!("{dp}:\n"));
		}

		// REM = 0
		out.push_str("\tli\t0 t4\n");
		store_word(out, "t4", rem_lo, 0);
		store_word(out, "t4", rem_hi, 0);

		for i in 0..32 {
			let no_bit = format!("{fn_name}_div32_{id}_p{i}");
			let keep = format!("{fn_name}_div32_{id}_k{i}");
			// REM = (REM << 1) | msb(NQ)   (32-bit shift-left of REM)
			out.push_str(&format!("\tlod\tt4 {rem_lo}(zero)\n"));
			out.push_str(&format!("\tlod\tt5 {rem_hi}(zero)\n"));
			out.push_str("\tsri\tt4 15 t6\n");
			out.push_str("\tsli\tt5 1 t5\n");
			out.push_str("\tor\tt5 t5 t6\n");
			out.push_str("\tsli\tt4 1 t4\n");
			// bring in bit 31 of NQ
			out.push_str(&format!("\tlod\tt6 {nq_hi}(zero)\n"));
			out.push_str(&format!("\tbgez\tt6 {no_bit}\n"));
			out.push_str("\tori\tt4 1 t4\n");
			out.push_str(&format!("{no_bit}:\n"));
			out.push_str(&format!("\tstr\tt4 {rem_lo}(zero)\n"));
			out.push_str(&format!("\tstr\tt5 {rem_hi}(zero)\n"));
			// NQ <<= 1  (32-bit)
			out.push_str(&format!("\tlod\tt4 {nq_lo}(zero)\n"));
			out.push_str(&format!("\tlod\tt5 {nq_hi}(zero)\n"));
			out.push_str("\tsri\tt4 15 t6\n");
			out.push_str("\tsli\tt5 1 t5\n");
			out.push_str("\tor\tt5 t5 t6\n");
			out.push_str("\tsli\tt4 1 t4\n");
			out.push_str(&format!("\tstr\tt4 {nq_lo}(zero)\n"));
			out.push_str(&format!("\tstr\tt5 {nq_hi}(zero)\n"));
			// if REM >=u D : REM -= D ; NQ |= 1
			cmp32_ge_unsigned(out, rem_lo, dd_lo, &keep, id, i);
			// REM -= D (32-bit)
			out.push_str(&format!("\tlod\tt4 {rem_lo}(zero)\n"));
			out.push_str(&format!("\tlod\tt5 {dd_lo}(zero)\n"));
			out.push_str("\tsub\tt4 t5 t4\n");
			out.push_str(&format!("\tstr\tt4 {rem_lo}(zero)\n"));
			out.push_str(&format!("\tlod\tt4 {rem_hi}(zero)\n"));
			out.push_str(&format!("\tlod\tt5 {dd_hi}(zero)\n"));
			out.push_str("\tsubf\tt4 t5 t4\n");
			out.push_str(&format!("\tstr\tt4 {rem_hi}(zero)\n"));
			out.push_str(&format!("\tlod\tt4 {nq_lo}(zero)\n"));
			out.push_str("\tori\tt4 1 t4\n");
			out.push_str(&format!("\tstr\tt4 {nq_lo}(zero)\n"));
			out.push_str(&format!("{keep}:\n"));
		}

		// pick result: NQ (quotient) or REM (remainder)
		let (res_lo, res_hi) = if want_rem { (rem_lo, rem_hi) } else { (nq_lo, nq_hi) };
		let _ = res_hi;
		// copy result into d
		out.push_str(&format!("\tlod\tt4 {res_lo}(zero)\n"));
		store_word(out, "t4", d, 0);
		out.push_str(&format!("\tlod\tt4 {}(zero)\n", res_lo + 1));
		store_word(out, "t4", d, 1);

		if signed {
			let flag = if want_rem { nsg } else { sgn };
			let pos = format!("{fn_name}_div32_{id}_rpos");
			out.push_str(&format!("\tlod\tt4 {flag}(zero)\n"));
			out.push_str(&format!("\tbeqz\tt4 {pos}\n"));
			neg_pair(out, d);
			out.push_str(&format!("{pos}:\n"));
		}
	}

	/// Branch to `keep` when the 32-bit unsigned value at slot `a` is
	/// **less than** the one at slot `b` (i.e. skip the subtract step).
	fn cmp32_ge_unsigned(out: &mut String, a: u16, b: u16, keep: &str, id: usize, i: u32) {
		let tie = format!("i32ge_{id}_{i}_tie");
		let done = format!("i32ge_{id}_{i}_done");
		out.push_str(&format!("\tlod\tt4 {}(zero)\n", a + 1));
		out.push_str(&format!("\tlod\tt5 {}(zero)\n", b + 1));
		out.push_str(&format!("\tbeq\tt4 t5 {tie}\n"));
		out.push_str(&format!("\tbltu\tt4 t5 {keep}\n")); // a.hi < b.hi -> a < b
		out.push_str(&format!("\tj\t{done}\n"));
		out.push_str(&format!("{tie}:\n"));
		out.push_str(&format!("\tlod\tt4 {}(zero)\n", a));
		out.push_str(&format!("\tlod\tt5 {}(zero)\n", b));
		out.push_str(&format!("\tbltu\tt4 t5 {keep}\n"));
		out.push_str(&format!("{done}:\n"));
	}

	#[cfg(test)]
	mod tests {
		//! Bit-exact models of the 32-bit multiply / divide loops, fuzzed
		//! against Rust. The emit code above is a mechanical transcription of
		//! these; they move together.

		fn sw_mul32(a: u32, b: u32) -> u32 {
			let (mut p, mut m, mut q) = (0u32, a, b);
			while q != 0 {
				if q & 1 != 0 {
					p = p.wrapping_add(m);
				}
				m <<= 1;
				q >>= 1;
			}
			p
		}

		fn sw_udiv32(n: u32, d: u32) -> (u32, u32) {
			let (mut rem, mut nq) = (0u32, n);
			for _ in 0..32 {
				rem = (rem << 1) | (nq >> 31);
				nq <<= 1;
				if rem >= d {
					rem -= d;
					nq |= 1;
				}
			}
			(nq, rem)
		}

		fn sw_sdiv32(n: i32, d: i32) -> (i32, i32) {
			let (q, r) = sw_udiv32(n.unsigned_abs(), d.unsigned_abs());
			let q = if (n < 0) != (d < 0) { (q as i32).wrapping_neg() } else { q as i32 };
			let r = if n < 0 { (r as i32).wrapping_neg() } else { r as i32 };
			(q, r)
		}

		fn probe() -> Vec<i64> {
			let mut v = vec![
				0, 1, 2, 3, 7, 100, 65535, 65536, 65537, 100_000, 1_000_000, 2_147_483_647,
				-1, -2, -7, -100_000, -2_147_483_648, i32::MIN as i64 + 1,
			];
			v.extend((0..40).map(|k| (k * 3_000_037i64) - 60_000_000));
			v
		}

		#[test]
		fn mul32_matches_rust() {
			for &a in &probe() {
				for &b in &probe() {
					let (a, b) = (a as u32, b as u32);
					assert_eq!(sw_mul32(a, b), a.wrapping_mul(b), "{a} * {b}");
				}
			}
		}

		#[test]
		fn div32_matches_rust() {
			for &n in &probe() {
				for &d in &probe() {
					if d == 0 {
						continue;
					}
					let (un, ud) = (n as u32, d as u32);
					assert_eq!(sw_udiv32(un, ud), (un / ud, un % ud), "{un} / {ud} u32");

					let (sn, sd) = (n as i32, d as i32);
					if sn == i32::MIN && sd == -1 {
						continue; // overflow, UB in the IR too
					}
					assert_eq!(sw_sdiv32(sn, sd), (sn / sd, sn % sd), "{sn} / {sd} i32");
				}
			}
		}
	}
}

/// Software multiply / divide / remainder.
///
/// The target CPU has an ALU (add/sub/bitwise), a 4-bit shifter, and
/// branches - but no multiplier or divider. LLVM's `Oz` only strength-
/// reduces `* 2^k` (to `shl`) at the IR level; every other `mul`/`udiv`/
/// `sdiv`/`urem`/`srem` reaches us intact, so we expand it here:
///
///   * `x * const`  -> a short chain of `sli` + `add` over the set bits
///     (or a shift + `neg` for a power of two).
///   * `x * y`       -> an unrolled shift-and-add loop (early-exits once the
///     multiplier is exhausted).
///   * `x / const` where const is an unsigned power of two -> `sri` / `andi`.
///   * everything else -> a 16-step restoring-division loop. The signed
///     forms divide the magnitudes and fix up the sign afterwards from the
///     original (still-live) operands.
///
/// Everything works within the three codegen scratch registers (t4-t6) plus
/// the instruction's own destination register - no stack, no runtime calls.
/// A proper `__mul` / `__udiv` routine reached via `jal` would be smaller,
/// but that needs the calling convention that doesn't exist yet.
///
/// 32-bit (`i32`) multiply/divide is *not* handled - the backend is 16-bit
/// throughout. Those hit a clear panic here rather than silently truncating.
mod muldiv {
	use super::{as_constant_i64, resolve_operand, RegAlloc};
	use inkwell::values::{AnyValue, AsValueRef, BasicValueEnum, InstructionValue};

	#[derive(Clone, Copy)]
	pub enum DivKind { UDiv, SDiv, URem, SRem }

	fn guard_width(instruction: InstructionValue, op: &str) {
		// i16 lands here; i32/u32 are handled upstream by `mod i32bit`. Only
		// something wider (i64 - LLVM's own pointer-index math) can reach this.
		let bits = instruction.get_type().into_int_type().get_bit_width();
		assert!(
			bits <= 16,
			"{op} on a {bits}-bit value has no lowering - i16/u16 and i32/u32 are \
			 supported, but not wider. This is almost certainly LLVM widening a \
			 pointer index; avoid the pointer arithmetic that produced it."
		);
	}

	fn dest_reg<'ctx>(instruction: InstructionValue<'ctx>, registers: &RegAlloc<'ctx>) -> String {
		registers.resolve(instruction.as_any_value_enum().try_into().unwrap())
	}

	/// `mv`/`li` `val` into the fixed register `reg`.
	fn load_into<'ctx>(out: &mut String, reg: &str, val: BasicValueEnum<'ctx>, registers: &RegAlloc<'ctx>) {
		if let Some(c) = as_constant_i64(val) {
			out.push_str(&format!("\tli\t{} {reg}\n", c as i16));
		} else {
			let src = registers.resolve(val);
			if src != reg {
				out.push_str(&format!("\tmv\t{src} {reg}\n"));
			}
		}
	}

	pub fn lower_mul<'ctx>(
		instruction: InstructionValue<'ctx>,
		registers: &mut RegAlloc<'ctx>,
		fn_name: &str,
	) -> String {
		guard_width(instruction, "multiply");
		let id = instruction.as_value_ref() as usize;
		let lhs = instruction.get_operand(0).unwrap().unwrap_value();
		let rhs = instruction.get_operand(1).unwrap().unwrap_value();
		let dest = dest_reg(instruction, registers);
		let mut out = String::new();

		// Both constant (shouldn't survive `Oz`, but be safe).
		if let (Some(x), Some(y)) = (as_constant_i64(lhs), as_constant_i64(rhs)) {
			out.push_str(&format!("\tli\t{} {dest}\n", (x as i16).wrapping_mul(y as i16)));
			return out;
		}

		// Canonicalise: variable operand in `a`, constant (if any) in `b`.
		let (a, b) = if as_constant_i64(lhs).is_some() { (rhs, lhs) } else { (lhs, rhs) };

		if let Some(c) = as_constant_i64(b) {
			if mul_by_const(&mut out, &dest, a, c as i16, registers) {
				return out;
			}
			// else: too many set bits - fall through to the generic loop.
		}

		// Generic: shift-and-add, MSB-agnostic, early-exit when the
		// multiplier reaches zero.
		let mcand = registers.next_scratch();
		let mplier = registers.next_scratch();
		let bit = registers.next_scratch();
		load_into(&mut out, &mcand, a, registers);
		load_into(&mut out, &mplier, b, registers);
		out.push_str(&format!("\tli\t0 {dest}\n"));

		let loop_l = format!("{fn_name}_mul{id}_loop");
		let skip_l = format!("{fn_name}_mul{id}_skip");
		let end_l = format!("{fn_name}_mul{id}_end");
		out.push_str(&format!("{loop_l}:\n"));
		out.push_str(&format!("\tbeqz\t{mplier} {end_l}\n"));
		out.push_str(&format!("\tandi\t{mplier} 1 {bit}\n"));
		out.push_str(&format!("\tbeqz\t{bit} {skip_l}\n"));
		out.push_str(&format!("\tadd\t{dest} {mcand} {dest}\n"));
		out.push_str(&format!("{skip_l}:\n"));
		out.push_str(&format!("\tsli\t{mcand} 1 {mcand}\n"));
		out.push_str(&format!("\tsri\t{mplier} 1 {mplier}\n"));
		out.push_str(&format!("\tj\t{loop_l}\n"));
		out.push_str(&format!("{end_l}:\n"));
		out
	}

	/// `dest = a * c` for a compile-time `c`. Returns `false` (emitting
	/// nothing) when `c` has too many set bits to be worth a shift chain -
	/// the caller then uses the generic loop.
	fn mul_by_const<'ctx>(
		out: &mut String,
		dest: &str,
		a: BasicValueEnum<'ctx>,
		c: i16,
		registers: &mut RegAlloc<'ctx>,
	) -> bool {
		if c == 0 {
			out.push_str(&format!("\tli\t0 {dest}\n"));
			return true;
		}
		let a_reg = registers.resolve(a);
		if c == 1 {
			if a_reg != dest {
				out.push_str(&format!("\tmv\t{a_reg} {dest}\n"));
			}
			return true;
		}

		let neg = c < 0;
		let mag = (c as i32).unsigned_abs();
		if mag.is_power_of_two() {
			let k = mag.trailing_zeros();
			out.push_str(&format!("\tsli\t{a_reg} {k} {dest}\n"));
			if neg {
				out.push_str(&format!("\tneg\t{dest} {dest}\n"));
			}
			return true;
		}

		// Sum of `a << k` over the set bits of the 16-bit product pattern.
		// (Using the raw bit pattern of `c` keeps the result correct mod
		// 2^16 even when `c` is negative.)
		let pattern = c as u16;
		let bits: Vec<u32> = (0..16).filter(|k| pattern & (1 << k) != 0).collect();
		if bits.len() > 6 {
			return false;
		}

		let acc = registers.next_scratch();
		let tmp = registers.next_scratch();
		let (first, rest) = bits.split_first().unwrap();
		if *first == 0 {
			out.push_str(&format!("\tmv\t{a_reg} {acc}\n"));
		} else {
			out.push_str(&format!("\tsli\t{a_reg} {first} {acc}\n"));
		}
		for &k in rest {
			if k == 0 {
				out.push_str(&format!("\tadd\t{acc} {a_reg} {acc}\n"));
			} else {
				out.push_str(&format!("\tsli\t{a_reg} {k} {tmp}\n"));
				out.push_str(&format!("\tadd\t{acc} {tmp} {acc}\n"));
			}
		}
		out.push_str(&format!("\tmv\t{acc} {dest}\n"));
		true
	}

	pub fn lower_div_rem<'ctx>(
		instruction: InstructionValue<'ctx>,
		registers: &mut RegAlloc<'ctx>,
		fn_name: &str,
		kind: DivKind,
	) -> String {
		guard_width(instruction, if matches!(kind, DivKind::URem | DivKind::SRem) { "remainder" } else { "divide" });
		let id = instruction.as_value_ref() as usize;
		let n = instruction.get_operand(0).unwrap().unwrap_value();
		let d = instruction.get_operand(1).unwrap().unwrap_value();
		let dest = dest_reg(instruction, registers);
		let signed = matches!(kind, DivKind::SDiv | DivKind::SRem);
		let want_rem = matches!(kind, DivKind::URem | DivKind::SRem);
		let mut out = String::new();

		let n_const = as_constant_i64(n).map(|c| c as i16);
		let d_const = as_constant_i64(d).map(|c| c as i16);

		if d_const == Some(0) {
			panic!("division by zero");
		}
		if let (Some(a), Some(b)) = (n_const, d_const) {
			let (a32, b32) = (a as i32, b as i32);
			let v = if want_rem {
				if signed { a32 % b32 } else { (a as u16 % b as u16) as i32 }
			} else if signed {
				a32 / b32
			} else {
				(a as u16 / b as u16) as i32
			};
			out.push_str(&format!("\tli\t{} {dest}\n", v as i16));
			return out;
		}

		// Fast path: unsigned divide/modulo by a power of two.
		if !signed {
			if let Some(c) = d_const {
				let cu = c as u16;
				if cu == 1 {
					let src = resolve_operand(registers, &mut out, n);
					if want_rem {
						out.push_str(&format!("\tli\t0 {dest}\n"));
					} else if src != dest {
						out.push_str(&format!("\tmv\t{src} {dest}\n"));
					}
					return out;
				}
				if cu.is_power_of_two() {
					let src = resolve_operand(registers, &mut out, n);
					if want_rem {
						out.push_str(&format!("\tandi\t{src} {} {dest}\n", cu - 1));
					} else {
						out.push_str(&format!("\tsri\t{src} {} {dest}\n", cu.trailing_zeros()));
					}
					return out;
				}
			}
		}

		// General case: 16-step restoring division on the magnitudes.
		let rem = registers.next_scratch();
		let nq = registers.next_scratch(); // dividend that shifts into the quotient
		let dv = registers.next_scratch();
		load_into(&mut out, &nq, n, registers);
		load_into(&mut out, &dv, d, registers);

		if signed {
			let ns = format!("{fn_name}_div{id}_nabs");
			let ds = format!("{fn_name}_div{id}_dabs");
			out.push_str(&format!("\tbgez\t{nq} {ns}\n"));
			out.push_str(&format!("\tneg\t{nq} {nq}\n"));
			out.push_str(&format!("{ns}:\n"));
			out.push_str(&format!("\tbgez\t{dv} {ds}\n"));
			out.push_str(&format!("\tneg\t{dv} {dv}\n"));
			out.push_str(&format!("{ds}:\n"));
		}
		out.push_str(&format!("\tli\t0 {rem}\n"));

		for i in 0..16 {
			let no_bit = format!("{fn_name}_div{id}_p{i}");
			let keep = format!("{fn_name}_div{id}_k{i}");
			// rem = (rem << 1) | msb(nq)
			out.push_str(&format!("\tsli\t{rem} 1 {rem}\n"));
			out.push_str(&format!("\tbgez\t{nq} {no_bit}\n"));
			out.push_str(&format!("\tori\t{rem} 1 {rem}\n"));
			out.push_str(&format!("{no_bit}:\n"));
			out.push_str(&format!("\tsli\t{nq} 1 {nq}\n"));
			// if rem >= divisor: rem -= divisor; quotient bit = 1
			out.push_str(&format!("\tbltu\t{rem} {dv} {keep}\n"));
			out.push_str(&format!("\tsub\t{rem} {dv} {rem}\n"));
			out.push_str(&format!("\tori\t{nq} 1 {nq}\n"));
			out.push_str(&format!("{keep}:\n"));
		}
		// nq = quotient, rem = remainder.
		let (result, spare) = if want_rem { (&rem, &nq) } else { (&nq, &rem) };

		if !signed {
			if result != &dest {
				out.push_str(&format!("\tmv\t{result} {dest}\n"));
			}
			return out;
		}

		// Signed fixup, from the original operands (never written by us).
		// `spare` (the result we don't want) is now a free scratch.
		if want_rem {
			// remainder takes the sign of the dividend
			apply_sign(&mut out, result, n_const, n, registers, &format!("{fn_name}_div{id}_rs"));
		} else {
			// quotient is negative iff exactly one operand was negative
			out.push_str(&format!("\tli\t0 {spare}\n"));
			flip_if_neg(&mut out, spare, n_const, n, registers, &format!("{fn_name}_div{id}_qn"));
			flip_if_neg(&mut out, spare, d_const, d, registers, &format!("{fn_name}_div{id}_qd"));
			let done = format!("{fn_name}_div{id}_qok");
			out.push_str(&format!("\tbeqz\t{spare} {done}\n"));
			out.push_str(&format!("\tneg\t{result} {result}\n"));
			out.push_str(&format!("{done}:\n"));
		}
		out.push_str(&format!("\tmv\t{result} {dest}\n"));
		out
	}

	/// `flag ^= (val < 0)`.
	fn flip_if_neg<'ctx>(
		out: &mut String,
		flag: &str,
		val_const: Option<i16>,
		val: BasicValueEnum<'ctx>,
		registers: &RegAlloc<'ctx>,
		label: &str,
	) {
		match val_const {
			Some(c) => {
				if c < 0 {
					out.push_str(&format!("\txori\t{flag} 1 {flag}\n"));
				}
			}
			None => {
				let reg = registers.resolve(val);
				out.push_str(&format!("\tbgez\t{reg} {label}\n"));
				out.push_str(&format!("\txori\t{flag} 1 {flag}\n"));
				out.push_str(&format!("{label}:\n"));
			}
		}
	}

	/// Negate `reg` when `val` is negative.
	fn apply_sign<'ctx>(
		out: &mut String,
		reg: &str,
		val_const: Option<i16>,
		val: BasicValueEnum<'ctx>,
		registers: &RegAlloc<'ctx>,
		label: &str,
	) {
		match val_const {
			Some(c) => {
				if c < 0 {
					out.push_str(&format!("\tneg\t{reg} {reg}\n"));
				}
			}
			None => {
				let src = registers.resolve(val);
				out.push_str(&format!("\tbgez\t{src} {label}\n"));
				out.push_str(&format!("\tneg\t{reg} {reg}\n"));
				out.push_str(&format!("{label}:\n"));
			}
		}
	}

	#[cfg(test)]
	mod tests {
		//! Bit-exact software models of the sequences `lower_mul` /
		//! `lower_div_rem` emit, fuzzed against Rust's own operators. If the
		//! asm ever drifts from these, the emit code and these models must
		//! move together.

		/// Mirrors the shift-and-add multiply loop.
		fn sw_mul(a: u16, b: u16) -> u16 {
			let mut mcand = a;
			let mut mplier = b;
			let mut acc: u16 = 0;
			while mplier != 0 {
				if mplier & 1 != 0 {
					acc = acc.wrapping_add(mcand);
				}
				mcand <<= 1;
				mplier >>= 1;
			}
			acc
		}

		/// Mirrors the 16-step restoring-division loop (unsigned).
		fn sw_udiv(n: u16, d: u16) -> (u16, u16) {
			let mut rem: u16 = 0;
			let mut nq = n;
			for _ in 0..16 {
				rem <<= 1;
				if nq & 0x8000 != 0 {
					rem |= 1;
				}
				nq <<= 1;
				if rem >= d {
					rem -= d;
					nq |= 1;
				}
			}
			(nq, rem) // (quotient, remainder)
		}

		/// Mirrors the signed wrapper: divide magnitudes, fix up the sign.
		fn sw_sdiv(n: i16, d: i16) -> (i16, i16) {
			let un = (n as i16).unsigned_abs();
			let ud = (d as i16).unsigned_abs();
			let (q, r) = sw_udiv(un, ud);
			let q = if (n < 0) != (d < 0) { (q as i16).wrapping_neg() } else { q as i16 };
			let r = if n < 0 { (r as i16).wrapping_neg() } else { r as i16 };
			(q, r)
		}

		fn probe() -> Vec<i32> {
			let mut v = vec![0, 1, 2, 3, 7, 8, 10, 15, 16, 100, 255, 256, 1000, 32767, -1, -2, -7, -10, -100, -32768];
			v.extend((0..64).map(|k| (k * 517) % 65536 - 30000));
			v
		}

		#[test]
		fn mul_matches_rust_wrapping() {
			for &a in &probe() {
				for &b in &probe() {
					let (a, b) = (a as u16, b as u16);
					assert_eq!(sw_mul(a, b), a.wrapping_mul(b), "{a} * {b}");
				}
			}
		}

		#[test]
		fn udiv_matches_rust() {
			for &n in &probe() {
				for &d in &probe() {
					let (n, d) = (n as u16, d as u16);
					if d == 0 {
						continue;
					}
					assert_eq!(sw_udiv(n, d), (n / d, n % d), "{n} / {d} (u16)");
				}
			}
		}

		#[test]
		fn sdiv_matches_rust_except_overflow() {
			for &n in &probe() {
				for &d in &probe() {
					let (n, d) = (n as i16, d as i16);
					if d == 0 || (n == i16::MIN && d == -1) {
						continue; // div-by-zero / signed overflow are UB in the IR too
					}
					assert_eq!(sw_sdiv(n, d), (n / d, n % d), "{n} / {d} (i16)");
				}
			}
		}
	}
}

/// `-Oz` treats a loop that never makes progress and has no side effects as
/// undefined behaviour: it replaces the whole thing with an empty `br` to
/// itself, which usually makes everything after it (I/O included) dead. The
/// symptom is "my program compiled to nothing". Catch it here with an error
/// that points at the real cause instead of silently emitting a spin loop.
fn reject_collapsed_infinite_loop(function: FunctionValue, name: &str) {
	for block in function.get_basic_block_iter() {
		// A block whose only non-phi instruction is `br` back to itself.
		let mut body = block
			.get_instructions()
			.filter(|i| i.get_opcode() != InstructionOpcode::Phi);
		let Some(term) = body.next() else { continue };
		if body.next().is_some() {
			continue; // has real work - an intentional `loop { draw() }` is fine
		}
		let branches_to_self = term.get_opcode() == InstructionOpcode::Br
			&& term.get_num_operands() == 1
			&& matches!(term.get_operand(0), Some(inkwell::values::Operand::Block(b)) if b == block);
		if branches_to_self {
			panic!(
				"`{name}` was optimised down to an empty infinite loop.\n\
				 That happens when a loop never terminates *and* does nothing \
				 observable - the optimiser is allowed to delete everything \
				 after it.\n\
				 Look for a `while` whose condition never becomes false (a \
				 missing `i += 1`?) with no `port_out` / `store` in its body."
			);
		}
	}
}

pub fn build(module: &Module) -> String {
	let mut out = String::new();

	let mut newline_function = false;

	// Program entry point: jump straight to main regardless of what order
	// functions happen to be emitted in. If main is emitted first, the
	// peephole pass below strips this back out on its own (fallthrough and
	// jump-to-immediately-next-label are equivalent), so no special-casing
	// needed either way.
	if module.get_function("main").is_some() {
		// Initialize stack pointer
		out.push_str(&format!("li\t{} sp\n", RAM_SIZE - 1));
		out.push_str("j\tmain\n");
		newline_function = true;
	}

	let config = RegisterConfig::load_default()
		.unwrap_or_else(|e| panic!("register allocation config: {e}"));

	for function in module.get_functions() {
		let name = function.get_name().to_str().unwrap();

		// Extern declaration (no body): nothing to emit, and no label - it
		// would just be dead. Covers the IO-port intrinsics (`__io_out` /
		// `__io_in`), which are lowered inline at their call sites.
		if function.count_basic_blocks() == 0 {
			continue;
		}

		reject_collapsed_infinite_loop(function, name);

		let is_main = name == "main";
		if newline_function {
			out.push_str("\n");
		}
		out.push_str(&format!("{name}:\n"));
		newline_function = true;

		let coloring = register_allocation::allocate_function(function, &config)
			.unwrap_or_else(|e| panic!("{e}"));

		let mut registers = RegAlloc::new(&coloring, &config);
		// Parameters arrive in the argument registers a0, a1, ...
		for (param, arg_reg) in function.get_param_iter().zip(config.arg_registers()) {
			registers.bind(param, &arg_reg.name);
		}

		let blocks: Vec<_> = function.get_basic_block_iter().collect();
		for (i, block) in blocks.iter().enumerate() {
			let block_name = block.get_name().to_str().unwrap();
			if !block_name.is_empty() && block_name != "entry" {
				out.push_str(&format!("{name}_{block_name}:\n"));
			}

			let next_block_name = blocks.get(i + 1)
				.and_then(|b| b.get_name().to_str().ok())
				.filter(|s| !s.is_empty())
				.map(|s| s.to_string());

			let instructions: Vec<InstructionValue> = block.get_instructions().collect();

			for instruction in &instructions {
				if instruction.get_opcode() == InstructionOpcode::Phi { continue; }
				// Phi copies for the block's successors are emitted by the
				// terminator (Br) itself, placed on the correct edge.
				registers.scratch_reset();
				out.push_str(&match_opcode(*instruction, &mut registers, name, next_block_name.as_deref(), is_main));
			}
		}
	}

	peephole(&out)
}

/// Cheap cleanup pass over the finished assembly text:
///   * drop `mv x x` (the allocator put a copy's source and destination in
///     the same register - common for zext/inttoptr and coalesced phis);
///   * drop `j L` when `L:` is the very next line.
fn peephole(asm: &str) -> String {
	let kept: Vec<&str> = asm
		.lines()
		.filter(|line| {
			let trimmed = line.trim();
			if let Some(rest) = trimmed.strip_prefix("mv\t") {
				let mut parts = rest.split_whitespace();
				if let (Some(src), Some(dest), None) = (parts.next(), parts.next(), parts.next()) {
					return src != dest;
				}
			}
			true
		})
		.collect();

	let mut out = Vec::with_capacity(kept.len());
	let mut i = 0;
	while i < kept.len() {
		let line = kept[i];
		if let Some(target) = line.trim().strip_prefix("j\t") {
			if let Some(next) = kept.get(i + 1) {
				if next.trim_end() == format!("{}:", target.trim()) {
					i += 1; // skip the jump, keep the label
					continue;
				}
			}
		}
		out.push(line);
		i += 1;
	}

	out.join("\n")
}

fn resolve_operand<'ctx>(regs: &mut RegAlloc<'ctx>, out: &mut String, val: BasicValueEnum<'ctx>) -> String {
	if let Some(const_val) = as_constant_i64(val) {
		if const_val == 0 {
			return "zero".to_string();
		}
		// Constants aren't register-allocated - materialise into a scratch
		// register, fresh at each use.
		let reg = regs.next_scratch();
		out.push_str(&format!("\tli\t{const_val} {reg}\n"));
		return reg;
	}
	regs.resolve(val)
}

/// Where a phi's incoming value comes from on a particular edge.
enum CopySource {
	Reg(String),
	/// A non-zero immediate.
	Imm(i64),
	Zero,
}

/// The simultaneous register copies that realise `succ`'s phi nodes for the
/// edge coming from `pred`. Returned as assembly text to be placed *on that
/// edge* - never before a conditional branch that might not take it.
fn phi_moves_for_edge<'ctx>(
	pred: BasicBlock<'ctx>,
	succ: BasicBlock<'ctx>,
	registers: &mut RegAlloc<'ctx>,
) -> String {
	registers.scratch_reset();

	let mut copies: Vec<(String, CopySource)> = Vec::new();
	let mut wide: Vec<(u16, BasicValueEnum<'ctx>)> = Vec::new(); // (dest slot, incoming)

	for instr in succ.get_instructions() {
		if instr.get_opcode() != InstructionOpcode::Phi {
			break; // phis are contiguous at the top of a block
		}
		let phi: inkwell::values::PhiValue = instr.try_into().unwrap();

		for i in 0..phi.count_incoming() {
			let Some((incoming_val, incoming_block)) = phi.get_incoming(i) else { continue; };
			if incoming_block != pred {
				continue;
			}

			// An undef incoming value supplies nothing defined for this phi
			// (e.g. a loop-local merged along the pre-header edge) - no move.
			if is_undef(incoming_val) {
				break;
			}

			if i32bit::involves_i32(instr) {
				let dst = registers.i32_slot(instr.as_any_value_enum().try_into().unwrap());
				wide.push((dst, incoming_val));
			} else {
				let dest = registers.resolve(instr.as_any_value_enum().try_into().unwrap());
				let source = match as_constant_i64(incoming_val) {
					Some(0) => CopySource::Zero,
					Some(v) => CopySource::Imm(v),
					None => CopySource::Reg(registers.resolve(incoming_val)),
				};
				copies.push((dest, source));
			}
			break; // one incoming edge per (phi, predecessor)
		}
	}

	let mut out = sequence_parallel_copy(copies, registers);
	out.push_str(&i32bit::emit_phi_copies(&wide, registers));
	out
}

/// Lower a set of copies that logically happen at once into a correct
/// sequence of `mv`/`li`. Register cycles (`a<-b, b<-a`) are broken with a
/// scratch register.
fn sequence_parallel_copy(copies: Vec<(String, CopySource)>, registers: &mut RegAlloc) -> String {
	// Copies already satisfied (destination already holds the source) drop out.
	let mut pending: Vec<(String, CopySource)> = copies
		.into_iter()
		.filter(|(dest, src)| !matches!(src, CopySource::Reg(r) if r == dest))
		.collect();

	let mut out = String::new();

	while !pending.is_empty() {
		let src_regs: HashSet<String> = pending
			.iter()
			.filter_map(|(_, src)| match src {
				CopySource::Reg(r) => Some(r.clone()),
				_ => None,
			})
			.collect();

		// A copy whose destination nobody else still needs as a source is
		// free to emit now.
		if let Some(pos) = pending.iter().position(|(dest, _)| !src_regs.contains(dest)) {
			let (dest, src) = pending.remove(pos);
			emit_copy(&mut out, &dest, &src);
			continue;
		}

		// Everything left is a register cycle. Park one destination's live
		// value in scratch and redirect whoever was reading it.
		let cycle_dest = pending[0].0.clone();
		let scratch = registers.next_scratch();
		out.push_str(&format!("\tmv\t{cycle_dest} {scratch}\n"));
		for (_, src) in pending.iter_mut() {
			if let CopySource::Reg(r) = src {
				if *r == cycle_dest {
					*r = scratch.clone();
				}
			}
		}
	}

	out
}

fn emit_copy(out: &mut String, dest: &str, src: &CopySource) {
	match src {
		CopySource::Reg(r) => out.push_str(&format!("\tmv\t{r} {dest}\n")),
		CopySource::Zero => out.push_str(&format!("\tmv\tzero {dest}\n")),
		CopySource::Imm(v) => out.push_str(&format!("\tli\t{v} {dest}\n")),
	}
}

/// Splits an address operand into (base register, constant offset).
/// If it's just a plain value with no addition involved, offset is 0.
/// If it's `add base, CONST` where CONST is a compile-time constant,
/// folds CONST into the addressing mode instead of materializing it
/// as a separate `li` + register.
fn resolve_address<'ctx>(regs: &mut RegAlloc<'ctx>, out: &mut String, addr: BasicValueEnum<'ctx>) -> (String, i64) {
	// Case 1: address is a plain constant int (e.g. a folded `first_free`)
	if let BasicValueEnum::IntValue(iv) = addr {
		if iv.is_const() {
			let val = iv.get_sign_extended_constant().unwrap();
			if val >= -(IMMEDIATE_MAX_VALUE_14 + 1) && val <= IMMEDIATE_MAX_VALUE_14 {
				return ("zero".to_string(), val);
			}
		}
	}

	// Case 2: address is a constant pointer (null, or inttoptr(N))
	if let BasicValueEnum::PointerValue(pv) = addr {
		if pv.is_null() {
			return ("zero".to_string(), 0);
		}
		if pv.is_const() {
			let text = pv.print_to_string().to_string();
			if let Some(val) = text.split("inttoptr (").nth(1)
				.and_then(|s| s.split_whitespace().nth(1))
				.and_then(|s| s.parse::<i64>().ok())
			{
				if val >= -(IMMEDIATE_MAX_VALUE_14 + 1) && val <= IMMEDIATE_MAX_VALUE_14 {
					return ("zero".to_string(), val);
				}
			}
		}
	}

	// Case 3: address is `base + CONST` (existing add-folding logic)
	if let BasicValueEnum::IntValue(iv) = addr {
		if let Some(instr) = iv.as_instruction() {
			if instr.get_opcode() == InstructionOpcode::Add {
				let lhs = instr.get_operand(0).unwrap().unwrap_value();
				let rhs = instr.get_operand(1).unwrap().unwrap_value();
				if let BasicValueEnum::IntValue(rhs_iv) = rhs {
					if rhs_iv.is_const() {
						let offset = rhs_iv.get_sign_extended_constant().unwrap();
						if offset >= -(IMMEDIATE_MAX_VALUE_14 + 1) && offset <= IMMEDIATE_MAX_VALUE_14 {
							let base_reg = resolve_operand(regs, out, lhs);
							return (base_reg, offset);
						}
					}
				}
			}
		}
	}

	// Fallback: nothing folds, resolve the whole address as a normal value
	(resolve_operand(regs, out, addr), 0)
}

/// Returns the constant integer value of a BasicValueEnum, if it IS one —
/// covers plain int constants, null pointers, and non-null constant
/// pointers (inttoptr(N)). Returns None for anything that isn't a
/// compile-time constant.
fn as_constant_i64(val: BasicValueEnum) -> Option<i64> {
	match val {
		// NOTE: is_const() is true for constant EXPRESSIONS too (e.g. a
		// ptrtoint/mul chain folded by an optimization pass), not just plain
		// integer literals — get_sign_extended_constant() only succeeds for
		// the latter, so this must stay an `if let`, never a bare `.unwrap()`.
		BasicValueEnum::IntValue(iv) if iv.is_const() => {
			iv.get_sign_extended_constant()
		}
		BasicValueEnum::PointerValue(pv) if pv.is_null() => Some(0),
		BasicValueEnum::PointerValue(pv) if pv.is_const() => {
			let text = pv.print_to_string().to_string();
			text.split("inttoptr (")
				.nth(1)
				.and_then(|s| s.split_whitespace().nth(1))
				.and_then(|s| s.parse().ok())
		}
		_ => None,
	}
}

fn is_fused_into_next_br(icmp: InstructionValue) -> bool {
	matches!(
        icmp.get_next_instruction().map(|n| n.get_opcode()),
        Some(InstructionOpcode::Br)
    )
}

fn materialize_icmp<'ctx>(icmp: InstructionValue<'ctx>, registers: &mut RegAlloc<'ctx>) -> String {
	let lhs = icmp.get_operand(0).unwrap().unwrap_value();
	let rhs = icmp.get_operand(1).unwrap().unwrap_value();
	let predicate = icmp.get_icmp_predicate().unwrap();

	let mut out = String::new();
	let lhs_reg = resolve_operand(registers, &mut out, lhs);
	let rhs_reg = resolve_operand(registers, &mut out, rhs);
	let dest = registers.resolve(icmp.as_any_value_enum().try_into().unwrap());

	// unique-ish labels; instruction address works fine as a disambiguator
	let id = icmp.as_any_value_enum().into_int_value().as_value_ref() as usize;
	let true_label = format!("icmp_true_{id}");
	let end_label = format!("icmp_end_{id}");

	let mnemonic = branch_mnemonic(predicate);
	out.push_str(&format!("\tli\t0 {dest}\n"));
	out.push_str(&format!("\t{mnemonic}\t{lhs_reg} {rhs_reg} {true_label}\n"));
	out.push_str(&format!("\tj\t{end_label}\n"));
	out.push_str(&format!("{true_label}:\n"));
	out.push_str(&format!("\tli\t1 {dest}\n"));
	out.push_str(&format!("{end_label}:\n"));
	out
}

/// Whether a value is LLVM's `undef` — a value that's compile-time constant
/// in the broad sense but has no defined bit pattern (nothing to move into
/// a register). Distinct from a normal zero/null constant.
fn is_undef(val: BasicValueEnum) -> bool {
	match val {
		BasicValueEnum::IntValue(iv) => iv.is_undef(),
		BasicValueEnum::PointerValue(pv) => pv.is_undef(),
		BasicValueEnum::FloatValue(fv) => fv.is_undef(),
		BasicValueEnum::ArrayValue(av) => av.is_undef(),
		BasicValueEnum::StructValue(sv) => sv.is_undef(),
		BasicValueEnum::VectorValue(vv) => vv.is_undef(),
		_ => { panic!("Unsupported `is_undef` {val}") }
	}
}

fn branch_mnemonic(predicate: IntPredicate) -> &'static str {
	match predicate {
		IntPredicate::EQ => "beq",
		IntPredicate::NE => "bne",
		IntPredicate::SGT => "bgt",  // Signed Greater Than
		IntPredicate::SGE => "bge",  // Signed Greater Than or Equal
		IntPredicate::SLT => "blt",  // Signed Less Than
		IntPredicate::SLE => "ble",  // Signed Less Than or Equal
		IntPredicate::UGT => "bgtu", // Unsigned Higher
		IntPredicate::UGE => "bgeu", // Unsigned Higher or Same
		IntPredicate::ULT => "bltu", // Unsigned Lower
		IntPredicate::ULE => "bleu", // Unsigned Lower or Same
	}
}

/// Logical negation of a predicate - used when the "then" block is the
/// natural fallthrough, so we can branch directly to "else" on the opposite
/// condition instead of emitting a dead extra jump.
fn invert_predicate(predicate: IntPredicate) -> IntPredicate {
	match predicate {
		IntPredicate::EQ => IntPredicate::NE,
		IntPredicate::NE => IntPredicate::EQ,
		IntPredicate::SGT => IntPredicate::SLE,
		IntPredicate::SGE => IntPredicate::SLT,
		IntPredicate::SLT => IntPredicate::SGE,
		IntPredicate::SLE => IntPredicate::SGT,
		IntPredicate::UGT => IntPredicate::ULE,
		IntPredicate::UGE => IntPredicate::ULT,
		IntPredicate::ULT => IntPredicate::UGE,
		IntPredicate::ULE => IntPredicate::UGT,
	}
}
#[cfg(test)]
mod tests {
	use super::build;
	use crate::compiler::codegen::{BuildMode, Codegen};
	use crate::compiler::lexer::tokenize;
	use crate::compiler::parser::Parser;
	use crate::compiler::typecheck::check_program;
	use inkwell::context::Context;

	/// Compile a source string end-to-end and return the generated assembly.
	fn asm_for_src(source: &str) -> String {
		let mut program = Parser::new(tokenize(source)).parse_program();
		check_program(&mut program);
		let ctx = Context::create();
		let mut codegen = Codegen::new(&ctx, "t", BuildMode::Release);
		codegen.lower_program(&program);
		crate::compiler::optimize(&codegen.module, "default<Oz>");
		codegen.module.verify().expect("module verifies");
		build(&codegen.module)
	}

	/// Compile, assemble, and run a source string headlessly; return RAM.
	fn run_src(source: &str) -> Vec<i16> {
		let asm = asm_for_src(source);
		let machine_code = crate::assembler::build::build(&asm).expect("assembles");
		crate::cpu::run_headless(&machine_code, 2_000_000)
	}

	/// Compile a source file the same way `compile_to_asm` does and return the
	/// generated assembly.
	fn asm_for(file: &str) -> String {
		let source = std::fs::read_to_string(format!("./src/compiler/programs/{file}.ecc")).unwrap();
		let tokens = tokenize(&source);
		let mut program = Parser::new(tokens).parse_program();
		check_program(&mut program);

		let ctx = Context::create();
		let mut codegen = Codegen::new(&ctx, file, BuildMode::Release);
		codegen.lower_program(&program);
		crate::compiler::optimize(&codegen.module, "default<Oz>");
		codegen.module.verify().expect("module verifies");

		build(&codegen.module)
	}

	/// Every operand of every emitted instruction must be a real machine
	/// register, `zero`, or an immediate - never a leftover SSA name or an
	/// empty string, and the self-move peephole must have run.
	fn assert_well_formed(asm: &str) {
		for line in asm.lines() {
			let line = line.trim();
			if line.is_empty() || line.ends_with(':') {
				continue;
			}
			let mut parts = line.split_whitespace();
			let mnemonic = parts.next().unwrap();

			if mnemonic == "mv" {
				let regs: Vec<&str> = parts.collect();
				assert_eq!(regs.len(), 2, "mv takes two operands: {line}");
				assert_ne!(regs[0], regs[1], "self-move survived the peephole: {line}");
			}

			for tok in line.split_whitespace().skip(1) {
				assert!(!tok.contains('%'), "un-lowered SSA operand in `{line}`");
			}
		}
	}

	#[test]
	fn fib_lowers_and_realises_the_loop_carried_phi_cycle() {
		let asm = asm_for("fib");
		assert_well_formed(&asm);

		// The while-loop rotates b <- a, a <- addtmp through shared registers,
		// which is a true parallel-copy cycle: it must be broken with a scratch
		// register (t4/t5/t6), not lowered as a naive sequence of `mv`s.
		let body = asm
			.split("while.body")
			.nth(1)
			.expect("fib has a while.body block");
		let cycle_break = body
			.lines()
			.take_while(|l| !l.trim_start().starts_with('j'))
			.any(|l| {
				let l = l.trim();
				l.starts_with("mv\t") && (l.contains(" t4") || l.contains(" t5") || l.contains(" t6"))
			});
		assert!(cycle_break, "loop-carried phi cycle was not broken with scratch:\n{asm}");
	}

	#[test]
	fn non_progressing_loop_is_rejected_with_a_useful_error() {
		// `while i < 10 {}` with nothing in the body and no `i` update: the
		// optimiser collapses it, which used to silently eat the rest of main.
		let err = std::panic::catch_unwind(|| {
			asm_for_src(
				"fn main() {\n\
				 \tlet mut i: i16 = 0;\n\
				 \twhile i < 10 {\n\
				 \t\tlet x = i + 1;\n\
				 \t}\n\
				 \tstore(0, 42);\n\
				 }",
			)
		})
		.unwrap_err();
		let msg = err
			.downcast_ref::<String>()
			.cloned()
			.or_else(|| err.downcast_ref::<&str>().map(|s| s.to_string()))
			.unwrap_or_default();
		assert!(msg.contains("infinite loop"), "unhelpful message: {msg}");
	}

	#[test]
	fn infinite_loop_with_io_is_fine() {
		// A deliberate `loop { draw() }` must NOT be rejected.
		let asm = asm_for_src(
			"fn main() {\n\
			 \tlet mut i: i16 = 0;\n\
			 \twhile i < 10 {\n\
			 \t\tport_out(0, i);\n\
			 \t}\n\
			 }",
		);
		assert!(asm.contains("out\t"), "expected the io write to survive");
	}

	#[test]
	fn i32_arithmetic_runs_correctly() {
		// 100000 doesn't fit in 16 bits, so every op below genuinely needs
		// the two-word path.
		let ram = run_src(
			"fn main() {\n\
			 \tlet a: i32 = 100000;\n\
			 \tlet b: i32 = 7;\n\
			 \tstore(0, (a + b) as i16);\n\
			 \tstore(1, (a - b) as i16);\n\
			 \tstore(2, (a * b) as i16);\n\
			 \tstore(3, (a / b) as i16);\n\
			 \tstore(4, (a % b) as i16);\n\
			 \tstore(5, (a >> 4) as i16);\n\
			 \tstore(6, (a << 3) as i16);\n\
			 \tstore(7, (-a / b) as i16);\n\
			 \tstore(8, (-a % b) as i16);\n\
			 }",
		);
		assert_eq!(ram[0], -31065, "a + b");
		assert_eq!(ram[1], -31079, "a - b");
		assert_eq!(ram[2], -20896, "a * b (low 16)");
		assert_eq!(ram[3], 14285, "a / b");
		assert_eq!(ram[4], 5, "a % b");
		assert_eq!(ram[5], 6250, "a >> 4");
		assert_eq!(ram[6], 13568, "a << 3 (low 16)");
		assert_eq!(ram[7], -14285, "-a / b");
		assert_eq!(ram[8], -5, "-a % b");
	}

	#[test]
	fn i32_comparisons_and_loop_run_correctly() {
		let ram = run_src(
			"fn main() {\n\
			 \tlet a: i32 = 100000;\n\
			 \tlet b: i32 = 7;\n\
			 \tstore(0, (a < b) as i16);\n\
			 \tstore(1, (b < a) as i16);\n\
			 \tstore(2, (a == a) as i16);\n\
			 \tlet neg: i32 = 0 - a;\n\
			 \tstore(3, (neg < b) as i16);\n\
			 \tlet mut acc: i32 = 0;\n\
			 \tlet mut i: i32 = 0;\n\
			 \twhile i < 10 {\n\
			 \t\tacc = acc + a;\n\
			 \t\ti = i + 1;\n\
			 \t}\n\
			 \tstore(4, (acc >> 4) as i16);\n\
			 \tstore(5, (acc == 1000000) as i16);\n\
			 }",
		);
		assert_eq!(ram[0], 0, "100000 < 7");
		assert_eq!(ram[1], 1, "7 < 100000");
		assert_eq!(ram[2], 1, "a == a");
		assert_eq!(ram[3], 1, "-100000 < 7");
		assert_eq!(ram[4], -3036, "(10*100000) >> 4, low 16");
		assert_eq!(ram[5], 1, "acc == 1_000_000");
	}

	#[test]
	fn i32_shifts_bitwise_and_extends_run_correctly() {
		let ram = run_src(
			"fn main() {\n\
			 \tlet a: i32 = 100000;\n\
			 \tlet n: i32 = 0 - 100000;\n\
			 \tstore(0, (a << 16) as i16);\n\
			 \tstore(1, ((a << 20) >> 16) as i16);\n\
			 \tstore(2, (n >> 20) as i16);\n\
			 \tstore(3, (a & 61680) as i16);\n\
			 \tstore(4, (a | 15) as i16);\n\
			 \tstore(5, (a ^ a) as i16);\n\
			 \tlet w: i16 = -3;\n\
			 \tlet e: i32 = w as i32;\n\
			 \tstore(6, (e + 5) as i16);\n\
			 \tstore(7, (e >> 1) as i16);\n\
			 \tlet hi: i32 = (a * a) >> 16;\n\
			 \tstore(8, hi as i16);\n\
			 }",
		);
		assert_eq!(ram[0] as u16, 0, "100000 << 16 truncated to 16 bits");
		// (100000 << 20) as i32 wraps; >> 16 arithmetic
		let v = ((100000i32.wrapping_shl(20)) >> 16) as i16;
		assert_eq!(ram[1], v, "(a << 20) >> 16");
		assert_eq!(ram[2] as u16, ((-100000i32) >> 20) as u16, "-100000 >> 20 (arith)");
		assert_eq!(ram[3] as u16, (100000i32 as u16) & 61680, "a & 0xF0F0");
		assert_eq!(ram[4] as u16, (100000i32 as u16) | 15, "a | 0xF");
		assert_eq!(ram[5], 0, "a ^ a");
		assert_eq!(ram[6], 2, "(-3) + 5 via sext");
		assert_eq!(ram[7], -2, "(-3) >> 1 arith");
		assert_eq!(ram[8] as u16, ((100000i64 * 100000) >> 16) as u16, "(a*a) >> 16");
	}

	#[test]
	fn u32_unsigned_ops_run_correctly() {
		let ram = run_src(
			"fn main() {\n\
			 \tlet a: u32 = 4000000000;\n\
			 \tlet b: u32 = 3;\n\
			 \tstore(0, (a / b) as i16);\n\
			 \tstore(1, (a % b) as i16);\n\
			 \tstore(2, (a >> 20) as i16);\n\
			 \tstore(3, (a > b) as i16);\n\
			 }",
		);
		// 4_000_000_000 / 3 = 1_333_333_333  (low16 = 0x5555 = 21845)
		assert_eq!(ram[0] as u16, (1_333_333_333u32 & 0xFFFF) as u16);
		assert_eq!(ram[1], 1, "4_000_000_000 % 3");
		assert_eq!(ram[2] as u16, (4_000_000_000u32 >> 20) as u16);
		assert_eq!(ram[3], 1, "a > b unsigned");
	}

	#[test]
	fn arith_lowers_mul_div_rem_to_shift_code() {
		let asm = asm_for("arith");
		assert_well_formed(&asm);

		// No hardware mul/div mnemonics exist - the expansion must be pure
		// shift/add/branch.
		for line in asm.lines().map(str::trim) {
			let m = line.split_whitespace().next().unwrap_or("");
			assert!(
				!matches!(m, "mul" | "div" | "udiv" | "sdiv" | "rem" | "mulh"),
				"a multiply/divide instruction leaked into the asm: {line}"
			);
		}

		// `a * 10` -> two shifts and an add, not the generic loop.
		assert!(asm.contains("sli\t"), "expected shift-based multiply:\n{asm}");
		// The division loop and its unsigned compare must be present.
		assert!(asm.contains("bltu\t"), "expected the restoring-division compare:\n{asm}");

		// And the whole thing still has to assemble.
		let mc = crate::assembler::build::build(&asm).expect("arith assembles");
		assert!(mc.len() > 50, "unexpectedly short program: {}", mc.len());
	}

	#[test]
	fn io_port_builtins_lower_to_out_and_in() {
		let asm = asm_for("io_ports");
		assert_well_formed(&asm);

		// No dangling label for the `__io_out` / `__io_in` extern declarations.
		assert!(!asm.contains("__io_out:"), "extern decl leaked a label:\n{asm}");
		assert!(!asm.contains("__io_in:"), "extern decl leaked a label:\n{asm}");

		let ops: Vec<&str> = asm
			.lines()
			.map(str::trim)
			.filter(|l| l.starts_with("out\t") || l.starts_with("in\t"))
			.collect();

		// Every `out` / `in` targets a constant port in 0..=15, register first.
		for line in &ops {
			let rest = line.split_once('\t').unwrap().1;
			let (reg, port) = rest.split_once(' ').expect("`out`/`in` take two operands");
			assert!(!reg.is_empty() && !reg.contains('%'), "bad reg operand: {line}");
			let port: i64 = port.trim().parse().unwrap_or_else(|_| panic!("non-numeric port: {line}"));
			assert!((0..=15).contains(&port), "port out of range: {line}");
		}

		assert!(ops.iter().any(|l| l.starts_with("out\t")), "no `out` emitted:\n{asm}");
		assert!(ops.iter().any(|l| l.starts_with("in\t")), "no `in` emitted:\n{asm}");

		// The emitted text has to make it through the assembler, and `out`/`in`
		// both encode to the IO opcode class (low 5 bits == 8).
		let machine_code = crate::assembler::build::build(&asm).expect("io_ports assembles");
		let io_instrs = machine_code.iter().filter(|w| (*w & 0b11111) == 8).count();
		assert!(io_instrs >= 2, "expected `out`/`in` to encode as IO opcodes, got {io_instrs}");
	}

	#[test]
	fn malloc_places_phi_copies_on_their_edge() {
		let asm = asm_for("std_lib");
		assert_well_formed(&asm);

		// malloc's entry block ends in a conditional branch whose `then`
		// successor (common.ret) takes the constant 1 through a phi. That copy
		// must sit *after* the branch - on the edge - never hoisted above it
		// where the fall-through path to `if.end` would run it too.
		let malloc = asm.split("\nmalloc:").nth(1).expect("malloc emitted");
		let malloc = malloc.split("\nfree:").next().unwrap();

		let pre_branch: Vec<&str> = malloc
			.lines()
			.map(str::trim)
			.take_while(|l| {
				let m = l.split_whitespace().next().unwrap_or("");
				!(m == "j" || (m.starts_with('b') && m != "break"))
			})
			.collect();
		assert!(
			!pre_branch.iter().any(|l| l.starts_with("li\t1 ")),
			"phi constant was hoisted above the branch: {pre_branch:?}"
		);
		assert!(
			malloc.lines().any(|l| l.trim() == "li\t1 s0" || l.trim().starts_with("li\t1 ")),
			"expected the phi constant `1` to be materialised somewhere in malloc:\n{malloc}"
		);
	}
}
