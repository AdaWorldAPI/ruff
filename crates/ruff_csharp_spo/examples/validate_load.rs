fn main() {
    let ndjson = std::fs::read_to_string(std::env::args().nth(1).unwrap()).unwrap();
    match ruff_csharp_spo::load(&ndjson) {
        Ok(triples) => println!("LOAD OK: {} triples", triples.len()),
        Err(e) => { println!("LOAD FAILED: {e}"); std::process::exit(1); }
    }
}
