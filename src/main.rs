use std::collections::HashMap;
use std::env;
use std::io::stdout;
use std::path::PathBuf;

use minion::cli::cli_sessions;

fn main() {
    let args: Vec<String> = env::args().collect();
    let env_map: HashMap<String, String> = env::vars().collect();
    let env_file = env_map
        .get("MINION_ENV_FILE")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".env")));

    // `minion sessions [query]` short-circuits before the REPL — print and exit.
    if cli_sessions(&args[1..], &env_map, &mut stdout().lock()) {
        return;
    }

    let rt = minion::Runtime::build(&args, env_map, env_file.as_deref());
    println!("{}", minion::banner(&rt));
}
