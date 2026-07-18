use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use minion::Runtime;

fn main() {
    let args: Vec<String> = env::args().collect();
    let env_map: HashMap<String, String> = env::vars().collect();
    let env_file = env_map
        .get("MINION_ENV_FILE")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".env")));
    let rt = Runtime::build(&args, env_map, env_file.as_deref());
    println!("{}", minion::banner(&rt));
}
