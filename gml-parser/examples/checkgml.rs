//! Parses GML on stdin and says where it fails. A throwaway harness for checking generated code.
use std::io::Read;

fn main() {
    let mut source = Vec::new();
    std::io::stdin().read_to_end(&mut source).unwrap();
    match gml_parser::ast::AST::new(&source) {
        Ok(_) => println!("OK"),
        Err(error) => {
            println!("ERROR {:?}", error);
            std::process::exit(1);
        },
    }
}
