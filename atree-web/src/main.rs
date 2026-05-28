//! `atree-web` — Start the ATree web server for visual code intelligence.
//!
//! Usage:
//!   atree-web --db <path> [--port 3020] [--root <repo_path>]

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    let mut db_path = None;
    let mut port = 3020u16;
    let mut repo_path = ".".to_string();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                if i < args.len() { db_path = Some(PathBuf::from(&args[i])); }
            }
            "--port" | "-p" => {
                i += 1;
                if i < args.len() { port = args[i].parse().unwrap_or(3020); }
            }
            "--root" | "-r" => {
                i += 1;
                if i < args.len() { repo_path = args[i].clone(); }
            }
            "--help" | "-h" => {
                println!("atree-web — Visual code intelligence graph server");
                println!();
                println!("Usage: atree-web [OPTIONS]");
                println!();
                println!("Options:");
                println!("  --db <PATH>       Path to SQLite index (required)");
                println!("  --port <N>        Port to listen on (default: 3020)");
                println!("  --root <PATH>     Repository root path (default: .)");
                println!("  --help, -h        Show this help");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    let db_path = db_path.unwrap_or_else(|| {
        eprintln!("Error: --db <PATH> is required. Point to your .atree/index.sqlite.");
        eprintln!();
        eprintln!("  Create an index first:  atree --semantic --db .atree/index.sqlite --root .");
        eprintln!("  Start the web server:   atree-web --db .atree/index.sqlite");
        std::process::exit(1);
    });

    if !db_path.exists() {
        eprintln!("Error: Index file not found at {}", db_path.display());
        eprintln!("  Create one first: atree --semantic --db {} --root .", db_path.display());
        std::process::exit(1);
    }

    log::info!("Starting atree-web: db={}, port={}", db_path.display(), port);

    if let Err(e) = atree_web::run(db_path, repo_path, port).await {
        eprintln!("Server error: {}", e);
        std::process::exit(1);
    }
}
