mod lexer;
mod ast;
pub mod parser;
mod typecheck;

#[test]
fn test_parsing(){
	use crate::compiler::{lexer::tokenize, parser::Parser};
	use std::fs;

	//

	let path = "./src/compiler/programs/fib.ecc";
	let source = fs::read_to_string(path).expect("Failed to read");

	let tokens = tokenize(&source);
	let mut parser = Parser::new(tokens);
	let program = parser.parse_program();

	println!("{:#?}", program);
}