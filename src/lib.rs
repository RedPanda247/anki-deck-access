use rusqlite::{Connection, Transaction, params};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AnkiDeck {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Flashcard {
    pub card_id: i64,
    pub deck_id: i64,
    /// Clean, separated fields (e.g., fields[0] is Front, fields[1] is Back)
    pub fields: Vec<String>,
    pub tags: Vec<String>,
    pub queue_state: i32,
}

/// Contains the info of where the database and media assets for the decks are stored in the file system
pub struct DeckDatabaseEnvironment {
    db_path: PathBuf,
    media_dir: PathBuf,
}

/// Contains a connection to the deck database
pub struct DatabaseConnection {
    conn: Connection,
}

impl DatabaseConnection {
    /// Consumes this wrapper and returns the underlying rusqlite connection.
    pub fn into_connection(self) -> Connection {
        self.conn
    }

    /// Retrieves a list of all decks stored inside the database by parsing the `col` JSON
    pub fn get_all_decks(&self) -> Result<Vec<AnkiDeck>, Box<dyn std::error::Error>> {
        let conn = &self.conn;
        let mut stmt = conn.prepare("SELECT decks FROM col LIMIT 1")?;

        let json_str: String = stmt.query_row([], |row| row.get(0))?;

        // Anki stores decks as a JSON map: {"id_string": {"name": "Deck Name", ...}}
        #[derive(Deserialize)]
        struct AnkiDeckRaw {
            name: String,
        }
        let raw_decks: HashMap<String, AnkiDeckRaw> = serde_json::from_str(&json_str)?;

        let decks = raw_decks
            .into_iter()
            .filter_map(|(id_str, raw)| {
                id_str
                    .parse::<i64>()
                    .ok()
                    .map(|id| AnkiDeck { id, name: raw.name })
            })
            .collect();

        Ok(decks)
    }
}

const DB_SETUP_SQL: &str = "
    CREATE TABLE IF NOT EXISTS col (id integer primary key, crt integer, mod integer, scm integer, ver integer, dats integer, usn integer, ls integer, conf text, models text, decks text, dconf text, tags text);
    CREATE TABLE IF NOT EXISTS notes (id integer primary key, guid text, mid integer, mod integer, usn integer, tags text, flds text, sfld integer, csum integer, flags integer, data text);
    CREATE TABLE IF NOT EXISTS cards (id integer primary key, nid integer, did integer, ord integer, mod integer, usn integer, type integer, queue integer, due integer, ivl integer, factor integer, reps integer, lapses integer, left integer, odue integer, odid integer, flags integer, data text);
    ";

impl DeckDatabaseEnvironment {
    /// Creates a new empty DeckDatabaseEnvironment in the specified directory if one does not already exist
    pub fn init<P: AsRef<Path>>(base_dir: P) -> Result<Self, Box<dyn std::error::Error>> {
        let base = base_dir.as_ref();

        let db_path = base.join("collection.db");
        let media_dir = base.join("media");

        fs::create_dir_all(&media_dir)?;

        // Open or create DB at path
        let mut conn = Connection::open(&db_path)?;

        // Configure DB connection for better performance and safer usage
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;

        // Add anki deck required tables and columns
        Self::add_deck_schema(&mut conn)?;

        Ok(Self { db_path, media_dir })
    }

    /// Merges an incoming .apkg file directly into this specific instance's database and media collection.
    pub fn import_deck<P: AsRef<Path>>(
        &self,
        apkg_path: P,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Temp dir for current process, not the device temp dir
        let temp_dir = std::env::temp_dir().join("anki_crate_extraction_temp");
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;

        // Extract ZIP payload
        let file = File::open(apkg_path.as_ref())?;
        let mut archive = zip::ZipArchive::new(file)?;
        archive.extract(&temp_dir)?;

        // Map and move media assets natively into our managed media directory
        let media_map_file = temp_dir.join("media");
        if media_map_file.exists() {
            let media_bytes = fs::read(&media_map_file)?;
            let media_map: HashMap<String, String> = serde_json::from_slice(&media_bytes)
                .or_else(|_| {
                    // Some decks may contain non-UTF8 bytes here; parse via lossy UTF-8 fallback.
                    let media_content = String::from_utf8_lossy(&media_bytes);
                    serde_json::from_str(&media_content)
                })
                .unwrap_or_default();
            for (temp_name, real_name) in media_map {
                let src = temp_dir.join(&temp_name);
                let dest = self.media_dir.join(&real_name);
                if src.exists() {
                    fs::copy(src, dest)?;
                }
            }
        }

        // Merge SQL collections securely
        let extracted_db_path = if temp_dir.join("collection.anki21b").exists() {
            let compressed_path = temp_dir.join("collection.anki21b");
            let decompressed_path = temp_dir.join("collection.anki21.sqlite");
            let compressed = File::open(compressed_path)?;
            let decompressed = zstd::decode_all(compressed)?;
            fs::write(&decompressed_path, decompressed)?;
            decompressed_path
        } else if temp_dir.join("collection.anki21").exists() {
            temp_dir.join("collection.anki21")
        } else {
            temp_dir.join("collection.anki2")
        };

        if extracted_db_path.exists() {
            let ext_conn = Connection::open(extracted_db_path)?;
            let mut master_conn = Connection::open(&self.db_path)?;

            let tx = master_conn.transaction()?;
            Self::merge_collections(&ext_conn, &tx)?;
            tx.commit()?;
        }

        fs::remove_dir_all(temp_dir)?;
        Ok(())
    }

    fn merge_collections(
        src: &Connection,
        dest_tx: &Transaction,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Look up notes from the extracted source
        let mut note_stmt = src.prepare(
            "SELECT id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data FROM notes",
        )?;
        let mut note_insert = dest_tx.prepare("
            INSERT OR IGNORE INTO notes (id, guid, mid, mod, usn, tags, flds, sfld, csum, flags, data) 
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
        ")?;

        let rows = note_stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, String>(10)?,
            ))
        })?;
        for row in rows {
            let r = row?;
            note_insert.execute((r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9, r.10))?;
        }

        // Look up cards from the extracted source
        let mut card_stmt = src.prepare("SELECT id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, lapses, left, odue, odid, flags, data FROM cards")?;
        let mut card_insert = dest_tx.prepare("
            INSERT OR IGNORE INTO cards (id, nid, did, ord, mod, usn, type, queue, due, ivl, factor, reps, lapses, left, odue, odid, flags, data)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ")?;

        let card_rows = card_stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, i64>(6)?,
                r.get::<_, i64>(7)?,
                r.get::<_, i64>(8)?,
                r.get::<_, i64>(9)?,
                r.get::<_, i64>(10)?,
                r.get::<_, i64>(11)?,
                r.get::<_, i64>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, i64>(14)?,
                r.get::<_, i64>(15)?,
                r.get::<_, i64>(16)?,
                r.get::<_, String>(17)?,
            ))
        })?;
        for card in card_rows {
            let c = card?;
            card_insert.execute(params![
                c.0, c.1, c.2, c.3, c.4, c.5, c.6, c.7, c.8, c.9, c.10, c.11, c.12, c.13, c.14,
                c.15, c.16, c.17,
            ])?;
        }

        Ok(())
    }

    fn add_deck_schema(conn: &mut Connection) -> Result<(), rusqlite::Error> {
        // Add anki deck required tables if not exist
        conn.execute_batch(DB_SETUP_SQL)?;
        Ok(())
    }

    /// Quick function for connecting to the database with rusqlite
    pub fn connect_to_database(&self) -> Result<DatabaseConnection, rusqlite::Error> {
        Ok(DatabaseConnection {
            conn: Connection::open(&self.db_path)?,
        })
    }
}

/// Quick function to extract an apkg file to directory for general testing an learning the library
/// In real usage you should use the init function to initialize the DeckDatabaseEnvironment
/// in desired directory and use import_deck when adding decks
pub fn extract_deck_to<P: AsRef<Path>>(
    destination_dir: P,
    apkg_path: P,
) -> Result<DeckDatabaseEnvironment, Box<dyn std::error::Error>> {
    // Init enviroment
    let enviroment = DeckDatabaseEnvironment::init(destination_dir)?;

    // Add deck to enviroment
    enviroment.import_deck(apkg_path)?;

    Ok(enviroment)
}
