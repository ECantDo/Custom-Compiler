use crate::cpu::UnsignedWord;

pub struct ProgramCounter {
	location: usize,
}

impl ProgramCounter {
	pub fn set(&mut self, value: UnsignedWord) {
		self.location = value as usize;
	}

	pub fn increment(&mut self) {
		self.location += 1;
	}

	pub fn get(&self) -> UnsignedWord {
		self.location as UnsignedWord
	}

	pub fn reset(&mut self) {
		self.location = 0;
	}
}

impl Default for ProgramCounter {
	fn default() -> Self {
		Self { location: 0 }
	}
}