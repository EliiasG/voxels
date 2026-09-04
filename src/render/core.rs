use std::sync::Arc;
use bevy::prelude::*;
use wgpu::{Adapter, Device, Instance, Queue};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

struct CoreRenderPlugin;

impl Plugin for CoreRenderPlugin {
    fn build(&self, app: &mut App) {
        app.set_runner(runner_fn);
    }
}

/// Bundles the four core wgpu resources into a single ECS resource so
/// systems that need a [Device] and a [Queue] only take one [Res] parameter.
/// Systems requiring just one of these still access via the named field.
#[derive(Resource)]
pub struct RenderContext {
    pub instance: Arc<Instance>,
    pub adapter: Adapter,
    pub device: Device,
    pub queue: Queue,
}

struct WinitApp {
    app: App,
    exit: Option<AppExit>,
}

impl ApplicationHandler for WinitApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        todo!()
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        todo!()
    }
}

fn runner_fn(mut app: App) -> AppExit {
    let mut winit_app = WinitApp { app, exit: None };
    EventLoop::new().expect("Failed to initialize event loop!").run_app(&mut winit_app).expect("Failed to start event loop!");
    winit_app.exit.expect("No exit")
}