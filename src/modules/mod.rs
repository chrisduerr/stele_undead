//! Stele bar modules.

use stele::calloop::LoopHandle;
use stele::{Alignment, Module, ModuleLayer, Size, State};

mod clock;
pub mod sway;

// SVG background layer contents.
//
// By storing the layer content, instead of the SVG data, we make sure the
// content IDs are shared. We count down from `u32::MAX` to avoid conflicts with
// auto-generated IDs.
pub mod svg_layers {
    use stele::LayerContent;

    pub const BG: LayerContent =
        LayerContent::Svg { id: u32::MAX, data: include_bytes!("../../data/bg.svg") };
    pub const BG_ALT: LayerContent =
        LayerContent::Svg { id: u32::MAX - 1, data: include_bytes!("../../data/bg_alt.svg") };
    pub const BG_HOVER: LayerContent =
        LayerContent::Svg { id: u32::MAX - 2, data: include_bytes!("../../data/bg_hover.svg") };

    pub const WS_EMPTY: LayerContent =
        LayerContent::Svg { id: u32::MAX - 3, data: include_bytes!("../../data/ws_empty.svg") };
    pub const WS_FULL: LayerContent =
        LayerContent::Svg { id: u32::MAX - 4, data: include_bytes!("../../data/ws_full.svg") };

    pub const BG_CORNER_LEFT: LayerContent = LayerContent::Svg {
        id: u32::MAX - 5,
        data: include_bytes!("../../data/bg_corner_left.svg"),
    };
    pub const BG_CORNER_RIGHT: LayerContent = LayerContent::Svg {
        id: u32::MAX - 6,
        data: include_bytes!("../../data/bg_corner_right.svg"),
    };
}

/// Register all modules.
pub fn register(event_loop: &LoopHandle<'static, State>, output_name: String) {
    sway::register(event_loop, output_name);
    clock::register(event_loop);
}

/// Get module for SVG corner borders.
pub fn corner_module(id: &str, alignment: Alignment, left: bool) -> Module {
    let svg = if left { svg_layers::BG_CORNER_LEFT } else { svg_layers::BG_CORNER_RIGHT };
    let mut corner = ModuleLayer::new(svg);
    corner.size = Size::new(35, 35);

    Module::new(id, alignment, vec![corner])
}
