use anki_deck_access::*;
use std::path::PathBuf;

fn main() {
    let path_to_apkg_file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets")
        .join("example_deck.apkg");

    // Temp dir is only used for functioning example, use own directory in real usage
    let example_directory = std::env::temp_dir().join("anki_envioment_temp");

    // You can use extract_deck_to for testing to quickly extraxt a packaged anki deck to be used in your project
    let anki_enviroment = extract_deck_to(
        example_directory, 
        path_to_apkg_file
    ).unwrap();

}
