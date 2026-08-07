use crate::cpu::{Long, UnsignedWord, Word};

pub struct ProgramCounter {
	location: usize,
}

impl ProgramCounter {
	pub fn set(&mut self, value: UnsignedWord) {
		self.location = value as usize;
	}
	
	pub fn add(&mut self, value: Word) {
		let new_loc = self.location as Long + value as Long;
		if new_loc < 0 {
			panic!("New location for PC is less than 0")
		}
		self.location = new_loc as usize;
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