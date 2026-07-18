use std::collections::HashMap;
use std::env;
use std::io::stdout;
use std::path::PathBuf;

use afi::cli::cli_sessions;
use afi::repl::run_repl;

fn main() {
    let args: Vec<String> = env::args().collect();
    let env_map: HashMap<String, String> = env::vars().collect();
    let env_file = env_map
        .get("MINION_ENV_FILE")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".env")));

    // `afi sessions [query]` short-circuits before the REPL - print and exit.
    if cli_sessions(&args[1..], &env_map, &mut stdout().lock()) {
        return;
    }

    let mut rt = afi::Runtime::build(&args, env_map.clone(), env_file.as_deref());

    // Run the REPL.
    run_repl(&mut rt, env_map);
}
