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
use stele::{Alignment, LayerContent, Module, ModuleLayer, State};
use tracing::error;

use crate::modules;
use crate::modules::svg_layers;
use crate::xdg::IconLoader;

/// Workspace icon size.
pub const ICON_SIZE: u32 = 24;

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
fn update_module(_: (), workspaces: &mut Vec<Workspace>, state: &mut State) {
    // Add left corner SVG module.
    let mut corner_left = modules::corner_module("ws_corner_left", Alignment::Center, true);
    corner_left.index = 0;
    state.update_module(corner_left);

    // Create background layers.
    let mut bg_layer = ModuleLayer::new(svg_layers::BG);
    bg_layer.size.width = 32;
    let mut bg_alt_layer = ModuleLayer::new(svg_layers::BG_ALT);
    bg_alt_layer.size.width = 32;

    // Add module for each workspace.
    for (i, workspace) in workspaces.iter().enumerate() {
        let bg_layer = if workspace.focused { bg_alt_layer.clone() } else { bg_layer.clone() };

        let mut ws_layer = ModuleLayer::new(workspace.icon.clone());
        ws_layer.size.width = ICON_SIZE;
        ws_layer.size.height = ICON_SIZE;

        let layers = vec![bg_layer, ws_layer];
        let mut module = Module::new(format!("ws_{i}"), Alignment::Center, layers);
        module.index = 1 + i as u8;

        state.update_module(module);
    }

    // Add right corner SVG module.
    let mut corner_right = modules::corner_module("ws_corner_right", Alignment::Center, false);
    corner_right.index = u8::MAX;
    state.update_module(corner_right);
}

/// Current workspace state.
#[derive(Clone)]
struct Workspace {
    icon: LayerContent,
    focused: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self { icon: svg_layers::WS_EMPTY, focused: Default::default() }
    }
}

/// Sway IPC calloop source.
struct SwayIpc {
    socket: Option<Generic<UnixStream>>,

    buffer: Vec<u8>,
    bytes_read: usize,

    workspaces: Vec<Workspace>,
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

            // Update icon and focus state based on child nodes.
            let (icon, focused) = self.workspace_state(workspace_node);
            let workspace: &mut Workspace = &mut self.workspaces[index];
            workspace.icon = icon.unwrap_or(svg_layers::WS_EMPTY);
            workspace.focused = focused;
        }
    }

    /// Get workspace icon and focus state.
    fn workspace_state(&mut self, node: Node) -> (Option<LayerContent>, bool) {
        let mut focused = false;
        let mut icon = None;

        // Check whether any child is focused or has a better icon.
        for node in node.nodes {
            let (child_icon, child_focused) = self.workspace_state(node);
            icon = icon.or(child_icon);
            focused |= child_focused;
        }

        // Check whether this node is focused or has an icon.
        if node.node_type == NodeType::Con
            && let Some(app_id) = node.app_id
        {
            match self.icon_loader.icon_path(&app_id) {
                Some(icon_path) => icon = Some(icon_path.to_path_buf().into()),
                None => icon = icon.or(Some(svg_layers::WS_FULL)),
            }
        }
        focused |= node.focused;

        (icon, focused)
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
    type Metadata = Vec<Workspace>;
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
                        callback((), &mut self.workspaces);
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
    #[serde(default)]
    focused: bool,

    nodes: Vec<Node>,
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
