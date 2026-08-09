use crate::compiler::codegen::Codegen;
use crate::compiler::lexer::tokenize;
use crate::compiler::parser::Parser;
use crate::compiler::typecheck::check_program;
use inkwell::context::Context;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::OptimizationLevel;
use std::fs;

mod lexer;
mod ast;
pub mod parser;
mod typecheck;
mod codegen;

const OUTPUT_DIR: &str = "compiled";


pub fn optimize(module: &inkwell::module::Module, opt_level: &str) {
	Target::initialize_native(&InitializationConfig::default())
		.expect("failed to initialize native target");

	let triple = TargetMachine::get_default_triple();
	let target = Target::from_triple(&triple).unwrap();
	let target_machine = target
		.create_target_machine(
			&triple,
			"generic",
			"",
			OptimizationLevel::Default,
			RelocMode::Default,
			CodeModel::Default,
		)
		.expect("failed to create target machine");

	let pass_options = PassBuilderOptions::create();
	module
		.run_passes(opt_level, &target_machine, pass_options)
		.expect("failed to run optimization passes");
}

pub fn compile_to_ll(path: &str) {
	// let path = "./src/compiler/programs/fib.ecc";
	let source = fs::read_to_string(path).expect("failed to read source file");

	let tokens = tokenize(&source);
	let mut parser = Parser::new(tokens);
	let mut program = parser.parse_program();

	check_program(&mut program);

	let context = Context::create();
	let mut codegen = Codegen::new(&context, "my_module");
	codegen.lower_program(&program);

	if let Err(e) = codegen.module.verify() {
		eprintln!("module is invalid: {}", e.to_string());
		return;
	}

	fs::create_dir_all(OUTPUT_DIR).expect("Error");
	codegen.module.print_to_file(format!("{}/out.unoptimized.ll", OUTPUT_DIR)).unwrap();
	optimize(&codegen.module, "default<O2>");
	codegen.module.print_to_file(format!("{}/out.ll", OUTPUT_DIR)).expect("failed to write .ll file");
	println!("wrote out.ll");
}

#[test]
fn test_parsing() {
	use crate::compiler::{lexer::tokenize, parser::Parser, typecheck::check_program};
	use std::fs;

	//

	let path = "./src/compiler/programs/fib.ecc";
	let source = fs::read_to_string(path).expect("Failed to read");

	let tokens = tokenize(&source);
	let mut parser = Parser::new(tokens);
	let mut program = parser.parse_program();

	check_program(&mut program);
	println!("{:#?}", program);
}