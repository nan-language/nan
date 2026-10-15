//! The syntax layer: lexing and parsing source files.

pub mod ast;
pub mod closing;
pub mod file;
mod fixes;
pub mod grammar;
mod idiom;
pub mod items;
pub mod lexer;
pub mod literal;
pub mod parser;
pub mod token;
pub mod tree;
