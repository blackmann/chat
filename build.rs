//! Builds the list of database migrations from `migrations/*.sql`, so adding
//! one is only adding a file. See `src/store/migrations.rs` for how they run.
use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("migrations");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut found = Vec::new();
    for path in fs::read_dir(&dir).expect("a migrations folder").flatten().map(|e| e.path()) {
        if !path.extension().is_some_and(|x| x == "sql") {
            continue;
        }
        let name = path.file_stem().unwrap().to_str().expect("a UTF-8 migration name").to_string();
        let number = name.split_once('_').and_then(|(n, _)| n.parse::<u64>().ok());
        let Some(number) = number else {
            panic!("migration {name}.sql should be named for when it was made and what it does, like 20261121143005_pin_threads.sql (scripts/new-migration.sh makes one)");
        };
        found.push((number, name, path));
    }
    found.sort();
    if let Some(w) = found.windows(2).find(|w| w[0].0 == w[1].0) {
        panic!("migrations {} and {} have the same time", w[0].1, w[1].1);
    }

    let mut list = String::from("pub const MIGRATIONS: &[(&str, &str)] = &[\n");
    for (_, name, path) in &found {
        list += &format!("    ({name:?}, include_str!({:?})),\n", path.display().to_string());
    }
    list += "];\n";
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("migrations.rs"), list).unwrap();
}
