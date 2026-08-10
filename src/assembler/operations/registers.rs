/// Port of `Assembler.Operations.Registers`.

pub const REGISTERS: [&str; 32] = [
	"r0", "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11", "r12", "r13", "r14",
	"r15", "r16", "r17", "r18", "r19", "r20", "r21", "r22", "r23", "r24", "r25", "r26", "r27",
	"r28", "r29", "r30", "r31",
];

pub const RISC_V_REGISTERS: [&str; 32] = [
	"zero", "ra", "sp", "gp", "tp", "s0", "s1", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7",
	"s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11", "t0", "t1", "t2", "t3", "t4",
	"t5", "t6",
];

/// Returns the register index, or `-1` if `register` doesn't match either
/// naming scheme. Kept as `i32` with a `-1` sentinel (rather than
/// `Option<usize>`) because `Build::assemble` relies on the sentinel being
/// fed straight into bitwise `& 0b11111` masking further down the pipeline -
/// changing this to `Option` would silently change that behavior.
pub fn get_register_value(register: &str) -> i32 {
	for i in 0..REGISTERS.len() {
		if REGISTERS[i] == register || RISC_V_REGISTERS[i] == register {
			return i as i32;
		}
	}
	-1
}
