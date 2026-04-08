use std::{env, process};

use modules::svg_layers;
use stele::{Alignment, Config, Module, State, Stele};
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
    modules::register(&stele.event_loop(), output_name.clone());

    // Show the bar.
    let config = config(stele.state(), output_name, true);
    stele.state().update_config(config);

    stele.run().unwrap();
}

/// Global bar configuration.
pub fn config(state: &mut State, output_name: String, workspace_empty: bool) -> Config {
    let mut config = Config::new();
    config.output = Some(output_name);
    config.size = Some(35);

    if workspace_empty {
        // Add corner for left modules.
        let mut corner_right = modules::corner_module("start_corner", Alignment::Start, false);
        corner_right.index = u8::MAX;
        state.update_module(corner_right);

        // Add corners for center modules.
        let mut corner_left = modules::corner_module("center_corner_left", Alignment::Center, true);
        corner_left.index = 0;
        state.update_module(corner_left);
        let mut corner_right =
            modules::corner_module("center_corner_right", Alignment::Center, false);
        corner_right.index = u8::MAX;
        state.update_module(corner_right);

        // Add corner for right modules.
        let mut corner_left = modules::corner_module("end_corner", Alignment::End, true);
        corner_left.index = 0;
        state.update_module(corner_left);

        // Remove background.
        state.update_module(Module::new("bg", Alignment::Start, Vec::new()));
    } else {
        // Remove all corners.
        state.update_module(Module::new("start_corner", Alignment::Start, Vec::new()));
        state.update_module(Module::new("center_corner_left", Alignment::Center, Vec::new()));
        state.update_module(Module::new("center_corner_right", Alignment::Center, Vec::new()));
        state.update_module(Module::new("end_corner", Alignment::End, Vec::new()));

        // Add contiguous background
        config.backgrounds = vec![svg_layers::BG];
    }

    config
}
