// Copyright 2016 Matthew Collins
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

#![recursion_limit = "300"]
#![allow(clippy::too_many_arguments)] // match standard gl functions with many arguments
#![allow(clippy::many_single_char_names)] // short variable names provide concise clarity
#![allow(clippy::float_cmp)] // float comparison used to check if changed

use instant::{Duration, Instant};
use log::{error, info, warn};
use std::fs;
extern crate steven_shared as shared;

use structopt::StructOpt;

extern crate steven_protocol;

pub mod ecs;
use steven_protocol::format;
use steven_protocol::nbt;
use steven_protocol::protocol;
pub mod gl;
use steven_protocol::types;
pub mod auth;
pub mod chunk_builder;
pub mod console;
pub mod entity;
pub mod model;
pub mod render;
pub mod resources;
pub mod screen;
pub mod server;
pub mod settings;
pub mod ui;
pub mod world;

use crate::protocol::mojang;
use cfg_if::cfg_if;
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

const CL_BRAND: console::CVar<String> = console::CVar {
    ty: PhantomData,
    name: "cl_brand",
    description: "cl_brand has the value of the clients current 'brand'. e.g. \"Steven\" or \
                  \"Vanilla\"",
    mutable: false,
    serializable: false,
    default: &|| "Steven".to_owned(),
};

pub struct Game {
    renderer: render::Renderer,
    screen_sys: screen::ScreenSystem,
    resource_manager: Arc<RwLock<resources::Manager>>,
    console: Arc<Mutex<console::Console>>,
    vars: Rc<console::Vars>,
    should_close: bool,

    server: server::Server,
    focused: bool,
    chunk_builder: chunk_builder::ChunkBuilder,

    connect_reply: Option<mpsc::Receiver<Result<server::Server, protocol::Error>>>,

    dpi_factor: f64,
    last_mouse_x: f64,
    last_mouse_y: f64,
    last_mouse_xrel: f64,
    last_mouse_yrel: f64,
    is_ctrl_pressed: bool,
    is_logo_pressed: bool,
    is_fullscreen: bool,
    default_protocol_version: i32,
}

impl Game {
    pub fn connect_to(&mut self, address: &str) {
        let (protocol_version, forge_mods, fml_network_version) =
            match protocol::Conn::new(address, self.default_protocol_version)
                .and_then(|conn| conn.do_status())
            {
                Ok(res) => {
                    info!(
                        "Detected server protocol version {}",
                        res.0.version.protocol
                    );
                    (
                        res.0.version.protocol,
                        res.0.forge_mods,
                        res.0.fml_network_version,
                    )
                }
                Err(err) => {
                    warn!(
                        "Error pinging server {} to get protocol version: {:?}, defaulting to {}",
                        address, err, self.default_protocol_version
                    );
                    (self.default_protocol_version, vec![], None)
                }
            };

        let (tx, rx) = mpsc::channel();
        self.connect_reply = Some(rx);
        let address = address.to_owned();
        let resources = self.resource_manager.clone();
        let profile = mojang::Profile {
            username: self.vars.get(auth::CL_USERNAME).clone(),
            id: self.vars.get(auth::CL_UUID).clone(),
            access_token: self.vars.get(auth::AUTH_TOKEN).clone(),
        };
        thread::spawn(move || {
            tx.send(server::Server::connect(
                resources,
                profile,
                &address,
                protocol_version,
                forge_mods,
                fml_network_version,
            ))
            .unwrap();
        });
    }

    pub fn tick(&mut self, delta: f64) {
        if !self.server.is_connected() {
            self.renderer.camera.yaw += 0.005 * delta;
            if self.renderer.camera.yaw > ::std::f64::consts::PI * 2.0 {
                self.renderer.camera.yaw = 0.0;
            }
        }

        if let Some(disconnect_reason) = self.server.disconnect_reason.take() {
            self.screen_sys
                .replace_screen(Box::new(screen::ServerList::new(Some(disconnect_reason))));
        }
        if !self.server.is_connected() {
            self.focused = false;
        }

        let mut clear_reply = false;
        if let Some(ref recv) = self.connect_reply {
            if let Ok(server) = recv.try_recv() {
                clear_reply = true;
                match server {
                    Ok(val) => {
                        self.screen_sys.pop_screen();
                        self.focused = true;
                        self.server.remove(&mut self.renderer);
                        self.server = val;
                    }
                    Err(err) => {
                        let msg = match err {
                            protocol::Error::Disconnect(val) => val,
                            err => {
                                let mut msg = format::TextComponent::new(&format!("{}", err));
                                msg.modifier.color = Some(format::Color::Red);
                                format::Component::Text(msg)
                            }
                        };
                        self.screen_sys
                            .replace_screen(Box::new(screen::ServerList::new(Some(msg))));
                    }
                }
            }
        }
        if clear_reply {
            self.connect_reply = None;
        }
    }
}

#[derive(StructOpt, Debug)]
#[structopt(name = "Stevenarella")]
struct Opt {
    /// Server to connect to
    #[structopt(short = "s", long = "server")]
    server: Option<String>,

    /// Username for offline servers
    #[structopt(short = "u", long = "username")]
    username: Option<String>,

    /// Log decoded packets received from network
    #[structopt(short = "n", long = "network-debug")]
    network_debug: bool,

    /// Parse a network packet from a file
    #[structopt(short = "N", long = "network-parse-packet")]
    network_parse_packet: Option<String>,

    /// Protocol version to use in the autodetection ping
    #[structopt(short = "p", long = "default-protocol-version")]
    default_protocol_version: Option<String>,
}

cfg_if! {
    if #[cfg(target_arch = "wasm32")] {
        extern crate console_error_panic_hook;
        pub use console_error_panic_hook::set_once as set_panic_hook;

        use wasm_bindgen::prelude::*;
        use wasm_bindgen::JsCast;

        #[wasm_bindgen]
        pub fn main() { main2(); }
    } else {
        #[inline]
        pub fn main() { main2(); }
    }
}

fn init_config_dir() {
    if std::path::Path::new("conf.cfg").exists() {
        return;
    }

    if let Some(mut path) = dirs::config_dir() {
        path.push("Stevenarella");
        if !path.exists() {
            std::fs::create_dir_all(path.clone()).unwrap();
        }
        std::env::set_current_dir(path).unwrap();
    }
}

fn main2() {
    #[cfg(target_arch = "wasm32")]
    set_panic_hook();

    init_config_dir();
    let opt = Opt::from_args();
    let con = Arc::new(Mutex::new(console::Console::new()));
    let proxy = console::ConsoleProxy::new(con.clone());

    log::set_boxed_logger(Box::new(proxy)).unwrap();
    log::set_max_level(log::LevelFilter::Trace);

    info!("Starting steven");

    let (vars, mut vsync) = {
        let mut vars = console::Vars::new();
        vars.register(CL_BRAND);
        console::register_vars(&mut vars);
        auth::register_vars(&mut vars);
        settings::register_vars(&mut vars);
        vars.load_config();
        vars.save_config();
        con.lock().unwrap().configure(&vars);
        let vsync = *vars.get(settings::R_VSYNC);
        (Rc::new(vars), vsync)
    };

    let (res, mut resui) = resources::Manager::new();
    let resource_manager = Arc::new(RwLock::new(res));

    let events_loop = winit::event_loop::EventLoop::new();

    let window_builder = winit::window::WindowBuilder::new()
        .with_title("Stevenarella")
        .with_inner_size(winit::dpi::LogicalSize::new(854.0, 480.0));

    #[cfg(target_arch = "wasm32")]
    let (context, shader_version, dpi_factor, winit_window) = {
        let winit_window = window_builder.build(&events_loop).unwrap();
        let dpi_factor = winit_window.scale_factor();

        use winit::platform::web::WindowExtWebSys;

        let canvas = winit_window.canvas();

        let html_window = web_sys::window().unwrap();
        let document = html_window.document().unwrap();
        let body = document.body().unwrap();

        body.append_child(&canvas)
            .expect("Append canvas to HTML body");

        let canvas = canvas.dyn_into::<web_sys::HtmlCanvasElement>().unwrap();
        let webgl2_context = canvas
            .get_context("webgl2")
            .expect("Failed to get WebGL2 context")
            .expect("Failed to create WebGL2 context, is WebGL2 support enabled? (https://get.webgl.org/webgl2/)")
            .dyn_into::<web_sys::WebGl2RenderingContext>()
            .unwrap();
        (
            glow::Context::from_webgl2_context(webgl2_context),
            "#version 300 es", // WebGL 2
            dpi_factor,
            winit_window,
        )
    };

    #[cfg(not(target_arch = "wasm32"))]
    let (context, shader_version, dpi_factor, glutin_window) = {
        let glutin_window = glutin::ContextBuilder::new()
            .with_stencil_buffer(0)
            .with_depth_buffer(24)
            .with_gl(glutin::GlRequest::GlThenGles {
                opengl_version: (3, 2),
                opengles_version: (3, 0),
            })
            .with_gl_profile(glutin::GlProfile::Core)
            .with_vsync(vsync)
            .build_windowed(window_builder, &events_loop)
            .expect("Could not create glutin window.");
        let dpi_factor = glutin_window.window().scale_factor();

        let glutin_window = unsafe {
            glutin_window
                .make_current()
                .expect("Could not set current context.")
        };

        let context = unsafe {
            glow::Context::from_loader_function(|s| glutin_window.get_proc_address(s) as *const _)
        };

        let shader_version = match glutin_window.get_api() {
            glutin::Api::OpenGl => "#version 150",      // OpenGL 3.2
            glutin::Api::OpenGlEs => "#version 300 es", // OpenGL ES 3.0 (similar to WebGL 2)
            glutin::Api::WebGl => {
                panic!("unexpectedly received WebGl API with glutin, expected to use glow codepath")
            }
        };

        (context, shader_version, dpi_factor, glutin_window)
    };

    gl::init(context);
    info!("Shader version: {}", shader_version);

    let renderer = render::Renderer::new(resource_manager.clone(), shader_version);
    let ui_container = ui::Container::new();

    let mut last_frame = Instant::now();

    let mut screen_sys = screen::ScreenSystem::new();
    if opt.server.is_none() {
        #[cfg(not(target_arch = "wasm32"))]
        {
            screen_sys.add_screen(Box::new(screen::Login::new(vars.clone())));
        }

        #[cfg(target_arch = "wasm32")]
        {
            screen_sys.add_screen(Box::new(screen::ServerList::new(None)));
        }
    }

    if let Some(username) = opt.username {
        vars.set(auth::CL_USERNAME, username);
    }

    let textures = renderer.get_textures();
    let default_protocol_version = protocol::versions::protocol_name_to_protocol_version(
        opt.default_protocol_version
            .unwrap_or_else(|| "".to_string()),
    );
    let mut game = Game {
        server: server::Server::dummy_server(resource_manager.clone()),
        focused: false,
        renderer,
        screen_sys,
        resource_manager: resource_manager.clone(),
        console: con,
        vars,
        should_close: false,
        chunk_builder: chunk_builder::ChunkBuilder::new(resource_manager, textures),
        connect_reply: None,
        dpi_factor,
        last_mouse_x: 0.0,
        last_mouse_y: 0.0,
        last_mouse_xrel: 0.0,
        last_mouse_yrel: 0.0,
        is_ctrl_pressed: false,
        is_logo_pressed: false,
        is_fullscreen: false,
        default_protocol_version,
    };
    game.renderer.camera.pos = cgmath::Point3::new(0.5, 13.2, 0.5);

    if opt.network_debug {
        protocol::enable_network_debug();
    }

    if let Some(filename) = opt.network_parse_packet {
        let data = fs::read(filename).unwrap();
        protocol::try_parse_packet(data, default_protocol_version);
        return;
    }

    if opt.server.is_some() {
        game.connect_to(&opt.server.unwrap());
    }

    let mut last_resource_version = 0;

    #[cfg(target_arch = "wasm32")]
    let winit_window = Rc::new(RefCell::new(winit_window));

    let game = Rc::new(RefCell::new(game));
    let ui_container = Rc::new(RefCell::new(ui_container));

    #[cfg(target_arch = "wasm32")]
    {
        let winit_window = Rc::clone(&winit_window);
        let game = Rc::clone(&game);
        let ui_container = Rc::clone(&ui_container);

        // Based on https://github.com/grovesNL/glow/blob/2d42c5b105d979efe764191b5b1ce78fab99ffcf/src/web_sys.rs#L3258
        fn request_animation_frame(f: &Closure<dyn FnMut(f64)>) {
            web_sys::window()
                .unwrap()
                .request_animation_frame(f.as_ref().unchecked_ref())
                .unwrap();
        }

        let f = Rc::new(RefCell::new(None));

        let mut last_timestamp = None;
        let mut running = true;

        *f.borrow_mut() = Some(Closure::wrap(Box::new({
            let f = f.clone();

            move |timestamp: f64| {
                let dt = last_timestamp.map_or(Duration::from_secs(0), |last_timestamp: f64| {
                    let dt_ms = (timestamp - last_timestamp).max(0.0);
                    let dt_secs = dt_ms / 1000.0;

                    Duration::from_secs_f64(dt_secs)
                });
                last_timestamp = Some(timestamp);

                let winit_window = winit_window.borrow_mut();
                let mut game = game.borrow_mut();
                let mut ui_container = ui_container.borrow_mut();

                tick_all(
                    &winit_window,
                    &mut game,
                    &mut ui_container,
                    &mut last_frame,
                    &mut resui,
                    &mut last_resource_version,
                    &mut vsync,
                );
                println!("render_loop");

                if !running {
                    let _ = f.borrow_mut().take();
                    return;
                }

                request_animation_frame(f.borrow().as_ref().unwrap());
            }
        }) as Box<dyn FnMut(f64)>));

        request_animation_frame(f.borrow().as_ref().unwrap());
    }

    #[cfg(target_arch = "wasm32")]
    let winit_window = Rc::clone(&winit_window);

    let game = Rc::clone(&game);
    let ui_container = Rc::clone(&ui_container);
    events_loop.run(move |event, _event_loop, control_flow| {
        #[cfg(target_arch = "wasm32")]
        let winit_window = winit_window.borrow_mut();

        #[cfg(not(target_arch = "wasm32"))]
        let winit_window = glutin_window.window();

        let mut game = game.borrow_mut();
        let mut ui_container = ui_container.borrow_mut();

        #[cfg(target_arch = "wasm32")]
        {
            *control_flow = winit::event_loop::ControlFlow::Wait;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            *control_flow = winit::event_loop::ControlFlow::Poll;
        }

        #[cfg(not(target_arch = "wasm32"))]
        if let winit::event::Event::WindowEvent {
            event: winit::event::WindowEvent::Resized(physical_size),
            ..
        } = event
        {
            glutin_window.resize(physical_size);
        }

        #[allow(clippy::needless_borrow)] // needless for native, not for web
        if !handle_window_event(&winit_window, &mut game, &mut ui_container, event) {
            return;
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            tick_all(
                winit_window,
                &mut game,
                &mut ui_container,
                &mut last_frame,
                &mut resui,
                &mut last_resource_version,
                &mut vsync,
            );

            glutin_window
                .swap_buffers()
                .expect("Failed to swap GL buffers");
        }

        if game.should_close {
            *control_flow = winit::event_loop::ControlFlow::Exit;
        }
    });
}

fn tick_all(
    window: &winit::window::Window,
    game: &mut Game,
    ui_container: &mut ui::Container,
    last_frame: &mut Instant,
    resui: &mut resources::ManagerUI,
    last_resource_version: &mut usize,
    vsync: &mut bool,
) {
    let now = Instant::now();
    let diff = now.duration_since(*last_frame);
    *last_frame = now;
    let frame_time = 1e9f64 / 60.0;
    let delta = (diff.subsec_nanos() as f64) / frame_time;
    let physical_size = window.inner_size();
    let (physical_width, physical_height) = physical_size.into();
    let (width, height) = physical_size.to_logical::<f64>(game.dpi_factor).into();

    let version = {
        let try_res = game.resource_manager.try_write();
        if let Ok(mut res) = try_res {
            res.tick(resui, ui_container, delta);
            res.version()
        } else {
            // TODO: why does game.resource_manager.write() sometimes deadlock?
            //warn!("Failed to obtain mutable reference to resource manager!");
            *last_resource_version
        }
    };
    *last_resource_version = version;

    let vsync_changed = *game.vars.get(settings::R_VSYNC);
    if *vsync != vsync_changed {
        error!("Changing vsync currently requires restarting");
        game.should_close = true;
        // TODO: after https://github.com/tomaka/glutin/issues/693 Allow changing vsync on a Window
        //vsync = vsync_changed;
    }
    let fps_cap = *game.vars.get(settings::R_MAX_FPS);

    game.tick(delta);
    game.server.tick(&mut game.renderer, delta);

    // Check if window is valid, it might be minimized
    if physical_width == 0 || physical_height == 0 {
        return;
    }

    game.renderer.update_camera(physical_width, physical_height);
    game.server.world.compute_render_list(&mut game.renderer);
    game.chunk_builder
        .tick(&mut game.server.world, &mut game.renderer, version);

    game.screen_sys
        .tick(delta, &mut game.renderer, ui_container);
    /* TODO: open console for chat messages
    if let Some(received_chat_at) = game.server.received_chat_at {
        if Instant::now().duration_since(received_chat_at).as_secs() < 5 {
            game.console.lock().unwrap().activate()
            // TODO: automatically deactivate the console after inactivity
        }
    }
    */
    game.console
        .lock()
        .unwrap()
        .tick(ui_container, &game.renderer, delta, width);
    ui_container.tick(&mut game.renderer, delta, width, height);
    game.renderer.tick(
        &mut game.server.world,
        delta,
        width as u32,
        height as u32,
        physical_width,
        physical_height,
    );

    if fps_cap > 0 && !*vsync {
        let frame_time = now.elapsed();
        let sleep_interval = Duration::from_millis(1000 / fps_cap as u64);
        if frame_time < sleep_interval {
            thread::sleep(sleep_interval - frame_time);
        }
    }
}

fn handle_window_event<T>(
    window: &winit::window::Window,
    game: &mut Game,
    ui_container: &mut ui::Container,
    event: winit::event::Event<T>,
) -> bool {
    use winit::event::*;
    match event {
        Event::MainEventsCleared => return true,
        Event::DeviceEvent {
            event: DeviceEvent::MouseMotion {
                delta: (xrel, yrel),
            },
            ..
        } => {
            let (rx, ry) = if xrel > 1000.0 || yrel > 1000.0 {
                // Heuristic for if we were passed an absolute value instead of relative
                // Workaround https://github.com/tomaka/glutin/issues/1084 MouseMotion event returns absolute instead of relative values, when running Linux in a VM
                // Note SDL2 had a hint to handle this scenario:
                // sdl2::hint::set_with_priority("SDL_MOUSE_RELATIVE_MODE_WARP", "1", &sdl2::hint::Hint::Override);
                let s = 8000.0 + 0.01;
                (
                    (xrel - game.last_mouse_xrel) / s,
                    (yrel - game.last_mouse_yrel) / s,
                )
            } else {
                let s = 2000.0 + 0.01;
                (xrel / s, yrel / s)
            };

            game.last_mouse_xrel = xrel;
            game.last_mouse_yrel = yrel;

            use std::f64::consts::PI;

            if game.focused {
                window.set_cursor_grab(true).unwrap();
                window.set_cursor_visible(false);
                if let Some(player) = game.server.player {
                    let rotation = game
                        .server
                        .entities
                        .get_component_mut(player, game.server.rotation)
                        .unwrap();
                    rotation.yaw -= rx;
                    rotation.pitch -= ry;
                    if rotation.pitch < (PI / 2.0) + 0.01 {
                        rotation.pitch = (PI / 2.0) + 0.01;
                    }
                    if rotation.pitch > (PI / 2.0) * 3.0 - 0.01 {
                        rotation.pitch = (PI / 2.0) * 3.0 - 0.01;
                    }
                }
            } else {
                window.set_cursor_grab(false).unwrap();
                window.set_cursor_visible(true);
            }
        }

        Event::WindowEvent { event, .. } => {
            match event {
                WindowEvent::ModifiersChanged(modifiers_state) => {
                    game.is_ctrl_pressed = modifiers_state.ctrl();
                    game.is_logo_pressed = modifiers_state.logo();
                }
                WindowEvent::CloseRequested => game.should_close = true,
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    game.dpi_factor = scale_factor;
                }

                WindowEvent::ReceivedCharacter(codepoint) => {
                    if !game.focused && !game.is_ctrl_pressed && !game.is_logo_pressed {
                        ui_container.key_type(game, codepoint);
                    }

                    #[cfg(target_os = "macos")]
                    if game.is_logo_pressed && codepoint == 'q' {
                        game.should_close = true;
                    }
                }

                WindowEvent::MouseInput { state, button, .. } => match (state, button) {
                    (ElementState::Released, MouseButton::Left) => {
                        let physical_size = window.inner_size();
                        let (width, height) =
                            physical_size.to_logical::<f64>(game.dpi_factor).into();

                        if game.server.is_connected()
                            && !game.focused
                            && !game.screen_sys.is_current_closable()
                        {
                            game.focused = true;
                            window.set_cursor_grab(true).unwrap();
                            window.set_cursor_visible(false);
                        } else if !game.focused {
                            #[cfg(not(target_arch = "wasm32"))]
                            // TODO: after Pointer Lock https://github.com/rust-windowing/winit/issues/1674
                            window.set_cursor_grab(false).unwrap();
                            window.set_cursor_visible(true);
                            ui_container.click_at(
                                game,
                                game.last_mouse_x,
                                game.last_mouse_y,
                                width,
                                height,
                            );
                        }
                    }
                    (ElementState::Pressed, MouseButton::Right) => {
                        if game.focused {
                            game.server.on_right_click(&mut game.renderer);
                        }
                    }
                    (_, _) => (),
                },
                WindowEvent::CursorMoved { position, .. } => {
                    let (x, y) = position.to_logical::<f64>(game.dpi_factor).into();
                    game.last_mouse_x = x;
                    game.last_mouse_y = y;

                    if !game.focused {
                        let physical_size = window.inner_size();
                        let (width, height) =
                            physical_size.to_logical::<f64>(game.dpi_factor).into();
                        ui_container.hover_at(game, x, y, width, height);
                    }
                }
                WindowEvent::MouseWheel { delta, .. } => {
                    // TODO: line vs pixel delta? does pixel scrolling (e.g. touchpad) need scaling?
                    match delta {
                        MouseScrollDelta::LineDelta(x, y) => {
                            game.screen_sys.on_scroll(x.into(), y.into());
                        }
                        MouseScrollDelta::PixelDelta(position) => {
                            let (x, y) = position.into();
                            game.screen_sys.on_scroll(x, y);
                        }
                    }
                }
                WindowEvent::KeyboardInput { input, .. } => {
                    match (input.state, input.virtual_keycode) {
                        (ElementState::Released, Some(VirtualKeyCode::Escape)) => {
                            if game.focused {
                                window.set_cursor_grab(false).unwrap();
                                window.set_cursor_visible(true);
                                game.focused = false;
                                game.screen_sys.replace_screen(Box::new(
                                    screen::SettingsMenu::new(game.vars.clone(), true),
                                ));
                            } else if game.screen_sys.is_current_closable() {
                                window.set_cursor_grab(true).unwrap();
                                window.set_cursor_visible(false);
                                game.focused = true;
                                game.screen_sys.pop_screen();
                            }
                        }
                        (ElementState::Pressed, Some(VirtualKeyCode::Grave)) => {
                            game.console.lock().unwrap().toggle();
                        }
                        (ElementState::Pressed, Some(VirtualKeyCode::F11)) => {
                            if !game.is_fullscreen {
                                // TODO: support options for exclusive and simple fullscreen
                                // see https://docs.rs/glutin/0.22.0-alpha5/glutin/window/struct.Window.html#method.set_fullscreen
                                window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(
                                    window.current_monitor(),
                                )));
                            } else {
                                window.set_fullscreen(None);
                            }

                            game.is_fullscreen = !game.is_fullscreen;
                        }
                        (ElementState::Pressed, Some(key)) => {
                            if game.focused {
                                if let Some(steven_key) =
                                    settings::Stevenkey::get_by_keycode(key, &game.vars)
                                {
                                    game.server.key_press(true, steven_key);
                                }
                            } else {
                                let ctrl_pressed = game.is_ctrl_pressed || game.is_logo_pressed;
                                ui_container.key_press(game, key, true, ctrl_pressed);
                            }
                        }
                        (ElementState::Released, Some(key)) => {
                            if game.focused {
                                if let Some(steven_key) =
                                    settings::Stevenkey::get_by_keycode(key, &game.vars)
                                {
                                    game.server.key_press(false, steven_key);
                                }
                            } else {
                                let ctrl_pressed = game.is_ctrl_pressed;
                                ui_container.key_press(game, key, false, ctrl_pressed);
                            }
                        }
                        (_, None) => (),
                    }
                }
                _ => (),
            }
        }

        _ => (),
    }

    false
}
