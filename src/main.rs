use std::{env, process};

use stele::{Config, Stele};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

mod modules;
mod xdg;

fn main() {
    // Get output name from args.
    let output_name = match env::args().nth(1) {
        Some(output_name) => output_name,
        None => {
            eprintln!("USAGE: stele_undead <OUTPUT_NAME>");
            process::exit(1);
        },
    };

    // Setup logging.
    let directives = env::var("RUST_LOG").unwrap_or("warn,stele=info,stele_undead=info".into());
    let env_filter = EnvFilter::builder().parse_lossy(directives);
    FmtSubscriber::builder().with_env_filter(env_filter).with_line_number(true).init();

    let mut stele = Stele::new().unwrap();

    // Register bar modules.
    modules::register(&stele.event_loop(), output_name);

    // Show the bar.
    stele.state().update_config(config());

    stele.run().unwrap();
}

/// Global bar configuration.
fn config() -> Config {
    let mut config = Config::new();
    config.size = Some(35);
    config
}
