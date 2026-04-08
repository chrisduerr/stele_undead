//! Sway workspaces.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::{env, mem};

use serde::Deserialize;
use stele::calloop::generic::Generic;
use stele::calloop::{
    self, EventSource, Interest, LoopHandle, Mode, Poll, PostAction, Readiness, Token, TokenFactory,
};
use stele::{Alignment, LayerContent, LayerModes, Module, ModuleLayer, Program, Size, State};
use tracing::error;

use crate::modules::svg_layers;
use crate::xdg::IconLoader;

/// Workspace icon size.
pub const ICON_SIZE: u32 = 24;

/// Priority for workspace icons using their `app_id`.
///
/// Icons with lower array index will be preferred. Icons in this list will be
/// prioritized over icons which aren't.
const ICON_PRIORITY: &[&str] = &["firefox"];

/// Number of workspaces rendered.
const WORKSPACE_COUNT: usize = 5;

/// Sway IPC magic string.
const MAGIC_STRING: &[u8] = b"i3-ipc";

/// Size of integer's in Sway's IPC header.
const SWAYINT_SIZE: usize = mem::size_of::<u32>();

/// Sway IPC header size (<magic-string><payload-length><payload-type>).
const HEADER_SIZE: usize = MAGIC_STRING.len() + SWAYINT_SIZE * 2;

/// Maximum IPC message buffer size.
const MAX_BUFFER_SIZE: usize = 100_000;

/// Add the clock and date modules to the bar.
pub fn register(event_loop: &LoopHandle<'static, State>, output_name: String) {
    event_loop.insert_source(SwayIpc::new(output_name), update_module).unwrap();
}

#[allow(clippy::ptr_arg)]
fn update_module(_: (), ipc: &mut SwayIpc, state: &mut State) {
    // Create background layers.
    let mut bg_layer = ModuleLayer::new(svg_layers::BG);
    bg_layer.size.width = 35;
    let mut bg_alt_layer = ModuleLayer::new(svg_layers::BG_ALT);
    bg_alt_layer.size.width = 35;
    let mut hover_bg_layer = ModuleLayer::new(svg_layers::BG_HOVER);
    hover_bg_layer.size.width = 35;
    hover_bg_layer.modes = LayerModes { default: false, hover: true, active: true };

    // Add module for each workspace.
    let mut focused_empty = false;
    for (i, workspace) in ipc.workspaces.iter().enumerate() {
        // Update whether there's currently any window visible.
        focused_empty |= workspace.focused && workspace.icon.is_none();

        let bg_layer = if workspace.focused { bg_alt_layer.clone() } else { bg_layer.clone() };

        let ws_icon = workspace.icon.clone().unwrap_or(svg_layers::WS_EMPTY);
        let mut ws_layer = ModuleLayer::new(ws_icon);
        ws_layer.size = Size::new(ICON_SIZE, ICON_SIZE);
        ws_layer.margin.bottom = 3;

        let layers = vec![bg_layer, hover_bg_layer.clone(), ws_layer];
        let mut module = Module::new(format!("ws_{i}"), Alignment::Center, layers);
        module.index = 1 + i as u8;

        // Switch to this workspace on click.
        let switch_cmd = format!("workspace {}-{i}", ipc.output_name);
        module.onclick = Some(Program { program: "swaymsg".into(), args: vec![switch_cmd] });

        state.update_module(module);
    }

    // Update bar config workspace emptiness changed.
    if focused_empty != ipc.last_focused_empty {
        ipc.last_focused_empty = focused_empty;
        let config = crate::config(state, String::new(), focused_empty);
        state.update_config(config);
    }
}

/// Current workspace state.
#[derive(Default, Clone)]
struct Workspace {
    icon: Option<LayerContent>,
    focused: bool,
}

/// Sway IPC calloop source.
struct SwayIpc {
    socket: Option<Generic<UnixStream>>,

    buffer: Vec<u8>,
    bytes_read: usize,

    workspaces: Vec<Workspace>,
    last_focused_empty: bool,
    output_name: String,

    icon_loader: IconLoader,
}

impl SwayIpc {
    fn new(output_name: String) -> Self {
        // Connect to the Unix socket.
        let socket_path = env::var("SWAYSOCK").expect("missing `SWAYSOCK` env");
        let mut socket =
            UnixStream::connect(Path::new(&socket_path)).expect("invalid Sway socket path");

        // Ensure we'll get `WouldBlock` when reading from an empty socket.
        socket.set_nonblocking(true).unwrap();

        // Initialize all workspaces as empty.
        let workspaces = vec![Workspace::default(); WORKSPACE_COUNT];

        // Subscribe to Sway events.
        let payload = br#"["workspace", "window"]"#;
        Self::ipc_write(&mut socket, PayloadType::Subscribe, payload).unwrap();

        // Get initial layout tree.
        Self::ipc_write(&mut socket, PayloadType::GetTree, &[]).unwrap();

        Self {
            output_name,
            workspaces,
            socket: Some(Generic::new(socket, Interest::READ, Mode::Level)),
            icon_loader: IconLoader::new(),
            buffer: vec![0; HEADER_SIZE],
            last_focused_empty: true,
            bytes_read: Default::default(),
        }
    }

    /// Attempt to parse current buffer as a Sway IPC message.
    fn parse_message(&mut self) -> Option<(PayloadType, &[u8])> {
        // Skip processing if header is not yet done.
        if self.bytes_read < HEADER_SIZE {
            return None;
        }

        // Ensure magic string is present.
        if &self.buffer[..MAGIC_STRING.len()] != MAGIC_STRING {
            self.bytes_read = 0;
            return None;
        }

        // Parse payload length.
        let mut offset = MAGIC_STRING.len();
        let payload_length_bytes = &self.buffer[offset..offset + SWAYINT_SIZE];
        let payload_length = u32::from_ne_bytes(payload_length_bytes.try_into().unwrap()) as usize;

        // Skip processing if payload is not yet done.
        let message_size = HEADER_SIZE + payload_length;
        if self.bytes_read < message_size {
            if self.buffer.len() < message_size && message_size <= MAX_BUFFER_SIZE {
                self.buffer.resize(message_size, 0);
            }
            return None;
        }

        // Parse payload type.
        offset += SWAYINT_SIZE;
        let payload_type_bytes = &self.buffer[offset..offset + SWAYINT_SIZE];
        let payload_type = u32::from_ne_bytes(payload_type_bytes.try_into().unwrap()).into();

        // Get payload data.
        offset += SWAYINT_SIZE;
        let payload = &self.buffer[offset..offset + payload_length];

        Some((payload_type, payload))
    }

    /// Update the workspace state.
    fn update_workspaces(&mut self, tree: Node) {
        // Ignore invalid tree nodes.
        if tree.node_type != NodeType::Root {
            error!("Invalid tree root: {:?}", tree.node_type);
            return;
        }

        // Reset all workspaces, since they might no longer exist.
        for workspace in &mut self.workspaces {
            *workspace = Workspace::default();
        }

        // Get output node from tree.
        let output_nodes = tree.nodes.into_iter();
        let output_node = output_nodes
            .filter(|node| node.node_type == NodeType::Output)
            .find(|node| node.name.as_ref() == Some(&self.output_name));
        let output_node = match output_node {
            Some(output_node) => output_node,
            None => {
                error!("Missing node for output {:?}", self.output_name);
                return;
            },
        };

        // Get focused workspace name.
        let focused_workspace = output_node.current_workspace;

        // Process workspace nodes.
        let workspace_nodes = output_node.nodes.into_iter();
        for workspace_node in workspace_nodes.filter(|node| node.node_type == NodeType::Workspace) {
            // Extract workspace index from its name.
            let workspace_name = workspace_node.name.as_ref();
            let index: usize = match workspace_name
                .and_then(|name| name.strip_prefix(&self.output_name))
                .and_then(|name| name.strip_prefix('-'))
                .and_then(|index| str::parse(index).ok())
            {
                Some(index) => index,
                None => continue,
            };

            // Update workspace focus,
            let workspace: &mut Workspace = &mut self.workspaces[index];
            workspace.focused = workspace_node.name == focused_workspace;

            // Update workspace icon.
            let icon = Self::workspace_icon(&mut self.icon_loader, workspace_node);
            workspace.icon = icon.map(|(_, icon)| icon);
        }
    }

    /// Determine workspace icon based on apps inside the workspace.
    ///
    /// The following priority is used to pick an icon:
    ///  - Lowest index in [`ICON_PRIORITY`]
    ///  - Order in the tree (earliest is preferred)
    ///  - WS_FULL if workspace contains any app
    ///  - WS_EMPTY
    fn workspace_icon(icon_loader: &mut IconLoader, node: Node) -> Option<(String, LayerContent)> {
        let mut icon = None;

        // For applications, immediately return the `app_id` and icon.
        let icon_name = node.app_id.or(node.window_properties.class);
        if node.node_type == NodeType::Con
            && let Some(icon_name) = icon_name
        {
            let icon = match icon_loader.icon_path(&icon_name) {
                Some(icon) => icon.to_path_buf().into(),
                None => svg_layers::WS_FULL,
            };
            return Some((icon_name, icon));
        }

        // For containers, check whether any child has a better icon.
        for node in node.nodes {
            // Get icon for this child node.
            let (child_icon_name, child_icon) = match Self::workspace_icon(icon_loader, node) {
                Some(child_icon) => child_icon,
                None => continue,
            };

            // Short-circuit if this is the first icon we've found.
            let (icon_name, icon) = match &mut icon {
                Some(icon) => icon,
                None => {
                    icon = Some((child_icon_name, child_icon));
                    continue;
                },
            };

            // Always replace built-in `WS_FULL` icons.
            if matches!(icon, LayerContent::Svg { .. }) {
                *icon_name = child_icon_name;
                *icon = child_icon;
                continue;
            }

            // Determine priority based on fallback list.
            let priority =
                ICON_PRIORITY.iter().position(|id| icon_name == id).unwrap_or(usize::MAX);
            let child_priority =
                ICON_PRIORITY.iter().position(|id| &child_icon_name == id).unwrap_or(usize::MAX);
            if child_priority < priority {
                *icon_name = child_icon_name;
                *icon = child_icon;
            }
        }

        icon
    }

    /// Write payload to the Sway socket.
    fn ipc_write(
        socket: &mut UnixStream,
        payload_type: PayloadType,
        payload: &[u8],
    ) -> Result<(), io::Error> {
        // Write magic string.
        socket.write_all(b"i3-ipc")?;

        // Write header.
        socket.write_all(&(payload.len() as u32).to_ne_bytes())?;
        socket.write_all(&payload_type.as_bytes())?;

        // Write payload and flush.
        socket.write_all(payload)?;
        socket.flush()?;

        Ok(())
    }
}

impl EventSource for SwayIpc {
    type Error = io::Error;
    type Event = ();
    type Metadata = SwayIpc;
    type Ret = ();

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> io::Result<PostAction>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let mut socket = self.socket.take().unwrap();
        let result = socket.process_events(readiness, token, |_, socket| {
            loop {
                // Read available bytes from the socket.
                let write_buffer = &mut self.buffer[self.bytes_read..];
                self.bytes_read += match unsafe { socket.get_mut().read(write_buffer) } {
                    Ok(0) => {
                        return Ok(PostAction::Continue);
                    },
                    Ok(bytes_read) => bytes_read,
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        return Ok(PostAction::Continue);
                    },
                    Err(err) => {
                        return Err(err);
                    },
                };

                // Try to parse the current buffer as IPC message.
                let (payload_type, payload) = match self.parse_message() {
                    Some(message) => message,
                    None => continue,
                };
                let message_len = HEADER_SIZE + payload.len();

                // Process IPC message.
                match payload_type {
                    // Process current Sway tree state.
                    PayloadType::GetTree => {
                        match serde_json::from_slice(payload) {
                            Ok(ipc_workspaces) => self.update_workspaces(ipc_workspaces),
                            Err(err) => error!("Failed to parse Sway tree: {err}"),
                        }

                        // Update modules.
                        callback((), self);
                    },
                    // Request full tree state on workspace/window changes.
                    PayloadType::EventWorkspace | PayloadType::EventWindow => {
                        let socket = unsafe { socket.get_mut() };
                        Self::ipc_write(socket, PayloadType::GetTree, &[]).unwrap();
                    },
                    PayloadType::Subscribe | PayloadType::Unknown => (),
                }

                // Remove parsed bytes from our buffer.
                self.buffer.rotate_left(message_len);
                self.bytes_read = self.bytes_read.saturating_sub(message_len);
            }
        });
        self.socket = Some(socket);

        result
    }

    fn register(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.socket.as_mut().unwrap().register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut Poll,
        token_factory: &mut TokenFactory,
    ) -> calloop::Result<()> {
        self.socket.as_mut().unwrap().reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.socket.as_mut().unwrap().unregister(poll)
    }
}

/// Sway tree node.
#[derive(Deserialize)]
struct Node {
    #[serde(rename = "type")]
    node_type: NodeType,

    name: Option<String>,
    app_id: Option<String>,
    current_workspace: Option<String>,

    #[serde(default)]
    window_properties: WindowProperties,

    nodes: Vec<Node>,
}

/// X11 window properties.
#[derive(Deserialize, Default)]
#[serde(default)]
struct WindowProperties {
    class: Option<String>,
}

#[derive(Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
enum NodeType {
    Root,
    Output,
    Workspace,
    Con,
}

/// Sway IPC message types.
#[repr(u32)]
#[derive(Copy, Clone, Debug)]
enum PayloadType {
    Subscribe = 2,
    GetTree = 4,
    EventWorkspace = 0x80000000,
    EventWindow = 0x80000003,
    Unknown = u32::MAX,
}

impl PayloadType {
    /// Get payload bytes in Sway IPC message format.
    fn as_bytes(&self) -> [u8; 4] {
        (*self as u32).to_ne_bytes()
    }
}

impl From<u32> for PayloadType {
    fn from(number: u32) -> Self {
        match number {
            2 => Self::Subscribe,
            4 => Self::GetTree,
            0x80000000 => Self::EventWorkspace,
            0x80000003 => Self::EventWindow,
            _ => {
                error!("Encountered unknown Sway IPC payload type: {number}");
                Self::Unknown
            },
        }
    }
}
