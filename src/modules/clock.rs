//! Time bar modules.

use std::time::{Duration, Instant};

use chrono::{Local, Timelike};
use stele::calloop::LoopHandle;
use stele::calloop::timer::{TimeoutAction, Timer};
use stele::{Alignment, Color, Margin, Module, ModuleLayer, State};

use crate::modules;
use crate::modules::svg_layers;

/// Add the clock and date modules to the bar.
pub fn register(event_loop: &LoopHandle<'static, State>) {
    event_loop.insert_source(Timer::immediate(), update_module).unwrap();
}

/// Update time module configuration.
fn update_module(_: Instant, _: &mut (), state: &mut State) -> TimeoutAction {
    // Use background color to size background SVG.
    let background = ModuleLayer::new(Color::new(24, 24, 24));
    let background_svg = ModuleLayer::new(svg_layers::BG);

    // Create date text layer.
    let now = Local::now();
    let date = now.format("%a. %-d").to_string();
    let mut date_layer = ModuleLayer::new(date);
    date_layer.margin = Margin::new(0, 25, 0, 25);

    // Add date module.
    let layers = vec![background.clone(), background_svg.clone(), date_layer];
    let mut module = Module::new("date_module", Alignment::Start, layers);
    module.index = 0;
    state.update_module(module);

    // Add date corner SVG module.
    let mut corner_right = modules::corner_module("date_corner", Alignment::Start, false);
    corner_right.index = 1;
    state.update_module(corner_right);

    // Add time corner SVG module.
    let mut corner_left = modules::corner_module("time_corner", Alignment::End, true);
    corner_left.index = 0;
    state.update_module(corner_left);

    // Create time text layer.
    let time = now.format("%H:%M").to_string();
    let mut time_layer = ModuleLayer::new(time);
    time_layer.margin = Margin::new(0, 25, 0, 25);

    // Add time module.
    let layers = vec![background, background_svg, time_layer];
    let mut module = Module::new("time_module", Alignment::End, layers);
    module.index = 1;
    state.update_module(module);

    // Wait until next minute change.
    let next_minute = Duration::from_secs((60 - now.second() + 1) as u64);
    TimeoutAction::ToDuration(next_minute)
}
