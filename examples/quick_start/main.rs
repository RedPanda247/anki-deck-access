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

    // Connect to the database with rusqlite
    let conn = anki_enviroment.connect_with_rusqlite().unwrap();

    // In Anki, note fields are stored in one string separated by unit separator (0x1F).
    fn front_and_back_from_flds(flds: &str) -> (String, String) {
        let mut parts = flds.split('\u{1f}');
        let front = parts.next().unwrap_or("").to_string();
        let back = parts.next().unwrap_or("").to_string();
        (front, back)
    }

    let mut stmt = conn
        .prepare(
            "SELECT n.flds
             FROM cards c
             JOIN notes n ON n.id = c.nid
             ORDER BY c.id",
        )
        .unwrap();

    let note_fields = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap();

    let mut fallback: Option<(String, String)> = None;
    let mut selected: Option<(String, String)> = None;

    for flds in note_fields {
        let flds = flds.unwrap();
        let (front, back) = front_and_back_from_flds(&flds);

        if fallback.is_none() {
            fallback = Some((front.clone(), back.clone()));
        }

        let is_warning = front.starts_with(
            "Please update to the latest Anki version, then import the .colpkg/.apkg file again.",
        );
        if !is_warning && !front.trim().is_empty() && !back.trim().is_empty() {
            selected = Some((front, back));
            break;
        }
    }

    match selected.or(fallback) {
        Some((front, back)) => {
            println!("Front: {}", front);
            println!("Back: {}", back);
        }
        None => println!("No cards found in this deck."),
    }
}
