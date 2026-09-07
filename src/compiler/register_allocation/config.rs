//! Loads and validates `registers.cfg` - the target's register file
//! description that drives register allocation.
//!
//! Format (see `registers.cfg` for the full comment):
//!
//! ```text
//! <name>   <class> [<class> ...]
//! ```
//!
//! `#` starts a comment, blank lines are skipped. Line order matters:
//! `arg` registers are handed out top-to-bottom, and that order is also
//! the allocator's colouring preference for the pool.

use crate::assembler::operations::registers::RISC_V_REGISTERS;
use std::fs;
use std::path::Path;

/// Default location, relative to the process working directory - same
/// convention as `OUTPUT_DIR` in `compiler/mod.rs`.
pub const DEFAULT_PATH: &str = "registers.cfg";

/// Whether a register is preserved across a call (`Callee`) or free to
/// clobber (`Caller`). `None` for `zero`/`reserved` registers, which are
/// never allocated at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveClass {
	Caller,
	Callee,
}

#[derive(Debug, Clone)]
pub struct RegisterInfo {
	pub name: String,
	/// Physical register number, i.e. index into `RISC_V_REGISTERS`.
	pub index: u8,
	pub is_zero: bool,
	pub is_reserved: bool,
	pub is_arg: bool,
	pub is_ret: bool,
	/// Kept out of the allocatable pool, but codegen may use it as a
	/// short-lived temporary (materialising constants, breaking parallel-copy
	/// cycles). Its value never has to survive a single instruction.
	pub is_scratch: bool,
	pub save: Option<SaveClass>,
}

impl RegisterInfo {
	/// A register the graph-colouring allocator may assign to a value.
	pub fn is_allocatable(&self) -> bool {
		!self.is_zero && !self.is_reserved && !self.is_scratch && self.save.is_some()
	}
}

#[derive(Debug, Clone)]
pub struct RegisterConfig {
	/// Every register, in the order it appeared in the file.
	pub registers: Vec<RegisterInfo>,
}

impl RegisterConfig {
	/// Read and parse the config at `path`.
	pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
		let path = path.as_ref();
		let text = fs::read_to_string(path)
			.map_err(|e| format!("could not read register config `{}`: {e}", path.display()))?;
		Self::parse(&text)
	}

	/// Load from [`DEFAULT_PATH`].
	pub fn load_default() -> Result<Self, String> {
		Self::load(DEFAULT_PATH)
	}

	pub fn parse(text: &str) -> Result<Self, String> {
		let mut registers = Vec::new();

		for (lineno, raw) in text.lines().enumerate() {
			let lineno = lineno + 1;

			// Strip trailing/whole-line comments, then whitespace.
			let line = raw.split('#').next().unwrap_or("").trim();
			if line.is_empty() {
				continue;
			}

			let mut tokens = line.split_whitespace();
			let name = tokens.next().expect("non-empty line has a first token");

			let index = RISC_V_REGISTERS
				.iter()
				.position(|r| *r == name)
				.ok_or_else(|| {
					format!("line {lineno}: `{name}` is not a known register name")
				})? as u8;

			if registers.iter().any(|r: &RegisterInfo| r.name == name) {
				return Err(format!("line {lineno}: register `{name}` listed twice"));
			}

			let mut info = RegisterInfo {
				name: name.to_string(),
				index,
				is_zero: false,
				is_reserved: false,
				is_arg: false,
				is_ret: false,
				is_scratch: false,
				save: None,
			};

			let mut saw_class = false;
			for class in tokens {
				saw_class = true;
				match class {
					"zero" => info.is_zero = true,
					"reserved" => info.is_reserved = true,
					"arg" => info.is_arg = true,
					"ret" => info.is_ret = true,
					"scratch" => info.is_scratch = true,
					"caller" => info.save = Some(SaveClass::Caller),
					"callee" => info.save = Some(SaveClass::Callee),
					other => {
						return Err(format!(
							"line {lineno}: unknown class `{other}` on register `{name}` \
							 (expected zero, reserved, arg, ret, scratch, caller or callee)"
						))
					}
				}
			}

			if !saw_class {
				return Err(format!("line {lineno}: register `{name}` has no class"));
			}
			if info.is_zero && (info.is_arg || info.is_ret || info.save.is_some()) {
				return Err(format!(
					"line {lineno}: `zero` register cannot also be arg/ret/caller/callee"
				));
			}
			if info.is_reserved && info.save.is_some() {
				return Err(format!(
					"line {lineno}: reserved register `{name}` cannot be in the allocatable pool"
				));
			}
			if info.is_scratch && (info.save.is_some() || info.is_zero || info.is_reserved) {
				return Err(format!(
					"line {lineno}: scratch register `{name}` cannot also be zero/reserved/caller/callee"
				));
			}

			registers.push(info);
		}

		let config = RegisterConfig { registers };

		if config.allocatable().count() == 0 {
			return Err("register config has no allocatable (caller/callee) registers".to_string());
		}
		if config.scratch_registers().count() == 0 {
			return Err("register config has no scratch registers (codegen needs at least one)".to_string());
		}

		Ok(config)
	}

	/// The pool the allocator colours with, in file order.
	pub fn allocatable(&self) -> impl Iterator<Item = &RegisterInfo> {
		self.registers.iter().filter(|r| r.is_allocatable())
	}

	/// Number of colours available (`k` for graph colouring).
	pub fn num_colors(&self) -> usize {
		self.allocatable().count()
	}

	/// Registers reserved for codegen scratch use, in file order.
	pub fn scratch_registers(&self) -> impl Iterator<Item = &RegisterInfo> {
		self.registers.iter().filter(|r| r.is_scratch)
	}

	/// Argument registers, in the order arguments are assigned to them.
	pub fn arg_registers(&self) -> impl Iterator<Item = &RegisterInfo> {
		self.registers.iter().filter(|r| r.is_arg)
	}

	pub fn get(&self, name: &str) -> Option<&RegisterInfo> {
		self.registers.iter().find(|r| r.name == name)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_the_checked_in_config() {
		let config = RegisterConfig::load_default().expect("registers.cfg should parse");

		// 8 arg registers a0..a7
		assert_eq!(config.arg_registers().count(), 8);
		assert_eq!(config.arg_registers().next().unwrap().name, "a0");

		// s0..s11 + t0..t3, all callee, all allocatable (t4..t6 held back as scratch)
		assert_eq!(config.num_colors(), 16);
		assert!(config.allocatable().all(|r| r.save == Some(SaveClass::Callee)));

		// t4..t6 are scratch: reserved for codegen, never in the pool
		assert_eq!(config.scratch_registers().count(), 3);
		assert_eq!(config.scratch_registers().next().unwrap().name, "t4");
		assert!(!config.get("t4").unwrap().is_allocatable());

		// a* stay out of the pool for now
		assert!(!config.get("a0").unwrap().is_allocatable());
		assert!(config.get("a0").unwrap().is_ret);

		assert!(config.get("zero").unwrap().is_zero);
		assert!(config.get("sp").unwrap().is_reserved);
	}

	#[test]
	fn rejects_unknown_register() {
		let err = RegisterConfig::parse("x9  callee").unwrap_err();
		assert!(err.contains("not a known register"));
	}

	#[test]
	fn rejects_unknown_class() {
		let err = RegisterConfig::parse("t0  volatile").unwrap_err();
		assert!(err.contains("unknown class"));
	}

	#[test]
	fn rejects_duplicate() {
		let err = RegisterConfig::parse("t0 callee\nt0 caller").unwrap_err();
		assert!(err.contains("listed twice"));
	}

	#[test]
	fn comments_and_blanks_are_ignored() {
		let config = RegisterConfig::parse(
			"# leading comment\n\n  t0  callee  # trailing\nt1 callee\nt2 scratch\n",
		)
		.unwrap();
		assert_eq!(config.num_colors(), 2);
	}
}
