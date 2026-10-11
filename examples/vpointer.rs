// Synthesize pointer + keyboard input on Wayland (Hyprland implements both).
// Usage (env must have WAYLAND_DISPLAY):
//   vpointer move X Y          # absolute move, logical compositor px
//   vpointer press|release left|right
//   vpointer click left|right
//   vpointer scroll DX DY      # axis delta in "length of vector" units (e.g. 120 0 is one notch)
//   vpointer type TOKENS...    # type text through zwp_virtual_keyboard (xkb us layout);
//                              # tokens: S-h = shift+h, C-b = ctrl+b; other tokens typed verbatim
use std::os::unix::io::AsRawFd as _;
use wayland_client::protocol::*;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1;
use wayland_protocols_wlr::virtual_pointer::v1::client::zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1;

mod vkb {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        use wayland_client::backend as wayland_backend;
        wayland_scanner::generate_interfaces!("examples/protocols/virtual-keyboard-unstable-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("examples/protocols/virtual-keyboard-unstable-v1.xml");
}
use vkb::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use vkb::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

const KEYMAP: &str = r#"xkb_keymap "vk" {
    xkb_keycodes { include "evdev" };
    xkb_types { include "complete" };
    xkb_compat { include "complete" };
    xkb_symbols { include "pc+us" };
};
"#;

#[derive(Default)]
struct State {
    pointer_man: Option<ZwlrVirtualPointerManagerV1>,
    keyboard_man: Option<ZwpVirtualKeyboardManagerV1>,
    seat: Option<wl_seat::WlSeat>,
    extents: (u32, u32),
}

macro_rules! noop_dispatch {
    ($t:ty) => {
        impl Dispatch<$t, ()> for State {
            fn event(
                _: &mut Self,
                _: &$t,
                event: <$t as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
                let _ = event;
            }
        }
    };
}

noop_dispatch!(ZwlrVirtualPointerManagerV1);
noop_dispatch!(ZwlrVirtualPointerV1);
noop_dispatch!(ZwpVirtualKeyboardManagerV1);
noop_dispatch!(ZwpVirtualKeyboardV1);
noop_dispatch!(wl_seat::WlSeat);

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            match interface.as_str() {
                "zwlr_virtual_pointer_manager_v1" => {
                    state.pointer_man =
                        Some(registry.bind::<ZwlrVirtualPointerManagerV1, (), State>(name, 2, qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    state.keyboard_man =
                        Some(registry.bind::<ZwpVirtualKeyboardManagerV1, (), State>(name, 1, qh, ()));
                }
                "wl_seat" => {
                    state.seat = Some(registry.bind::<wl_seat::WlSeat, (), State>(name, 1, qh, ()));
                }
                _ => {}
            }
            if std::env::var("VPOINTER_LIST").is_ok() {
                println!("global {name} {interface}");
            }
        }
    }
}

/// evdev us keycodes (xkb sends evdev + 8): (code, needs_shift)
fn key_for(c: char) -> Option<(u32, bool)> {
    let (code, shift) = match c {
        'a'..='z' => (
            match c {
                'q' => 16, 'w' => 17, 'e' => 18, 'r' => 19, 't' => 20, 'y' => 21, 'u' => 22,
                'i' => 23, 'o' => 24, 'p' => 25, 'a' => 30, 's' => 31, 'd' => 32, 'f' => 33,
                'g' => 34, 'h' => 35, 'j' => 36, 'k' => 37, 'l' => 38, 'z' => 44, 'x' => 45,
                'c' => 46, 'v' => 47, 'b' => 48, 'n' => 49, 'm' => 50,
                _ => unreachable!(),
            },
            false,
        ),
        'A'..='Z' => (key_for(c.to_ascii_lowercase()).map_or(0, |(c, _)| c), true),
        '1'..='9' => (c as u32 - '1' as u32 + 2, false),
        '0' => (11, false),
        '!' => (2, true), '@' => (3, true), '#' => (4, true), '$' => (5, true),
        '%' => (6, true), '^' => (7, true), '&' => (8, true), '*' => (9, true),
        '(' => (10, true), ')' => (11, true),
        '-' => (12, false), '_' => (12, true),
        '=' => (13, false), '+' => (13, true),
        ' ' => (57, false),
        '\t' => (15, false),
        '\n' => (28, false),
        ',' => (51, false), '<' => (51, true),
        '.' => (52, false), '>' => (52, true),
        '/' => (53, false), '?' => (53, true),
        ';' => (39, false), ':' => (39, true),
        '\'' => (40, false), '"' => (40, true),
        '[' => (26, false), '{' => (26, true),
        ']' => (27, false), '}' => (27, true),
        '\\' => (43, false), '|' => (43, true),
        '`' => (41, false), '~' => (41, true),
        _ => (300, false),
    };
    if code == 300 {
        return None;
    }
    Some((code, shift))
}

const SHIFT_KEY: u32 = 42; // evdev left shift
const CTRL_KEY: u32 = 29;  // evdev left ctrl
const SHIFT_MOD: u32 = 1 << 0;
const CTRL_MOD: u32 = 1 << 2;

fn now_ms() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_millis() as u32
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: vpointer move X Y | press|release|click B | scroll DX DY | type TOKENS...");
        std::process::exit(1);
    }
    let mut extents = (1440u32, 900u32);
    if let Ok(env) = std::env::var("VPOINTER_EXTENTS") {
        let mut it = env.split('x');
        extents = (
            it.next().unwrap_or("1440").parse().unwrap(),
            it.next().unwrap_or("900").parse().unwrap(),
        );
    }
    let conn = Connection::connect_to_env().expect("connect wayland");
    let mut queue = conn.new_event_queue::<State>();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut state = State { extents, ..Default::default() };
    queue.roundtrip(&mut state).unwrap();

    if args[1] == "type" {
        let vkm = state.keyboard_man.as_ref().expect("no zwp_virtual_keyboard_manager_v1");
        let vk = vkm.create_virtual_keyboard(state.seat.as_ref().expect("no wl_seat"), &qh, ());
        let mut f = tempfile_keymap();
        let (fd, size) = (f.as_raw_fd(), KEYMAP.len() as u32);
        vk.keymap(wl_keyboard::KeymapFormat::XkbV1, fd, size);
        queue.roundtrip(&mut state).unwrap();
        std::mem::drop(f);
        // typing: dispatch each token
        for tok in &args[2..] {
            if let Some(rest) = tok.strip_prefix("C-") {
                let (code, shift) = key_for(rest.chars().next().unwrap_or(' ')).unwrap_or((300, false));
                if code == 300 {
                    eprintln!("unknown key {tok}");
                    std::process::exit(1);
                }
                let t = now_ms();
                vk.key(t, CTRL_KEY + 8, wl_keyboard::KeyState::Pressed);
                vk.modifiers(now_ms(), CTRL_MOD, 0, 0, 0);
                queue.roundtrip(&mut state).unwrap();
                if shift { vk.key(now_ms(), SHIFT_KEY + 8, wl_keyboard::KeyState::Pressed); }
                vk.key(now_ms(), code + 8, wl_keyboard::KeyState::Pressed);
                queue.roundtrip(&mut state).unwrap();
                vk.key(now_ms(), code + 8, wl_keyboard::KeyState::Released);
                if shift { vk.key(now_ms(), SHIFT_KEY + 8, wl_keyboard::KeyState::Released); }
                vk.modifiers(now_ms(), 0, 0, 0, 0);
                let t = now_ms();
                vk.key(t, CTRL_KEY + 8, wl_keyboard::KeyState::Released);
                queue.roundtrip(&mut state).unwrap();
            } else if let Some(rest) = tok.strip_prefix("S-") {
                let c = rest.chars().next().unwrap_or(' ');
                let Some((code, _)) = key_for(c) else {
                    eprintln!("unknown key {tok}");
                    std::process::exit(1);
                };
                vk.key(now_ms(), SHIFT_KEY + 8, wl_keyboard::KeyState::Pressed);
                vk.modifiers(now_ms(), SHIFT_MOD, 0, 0, 0);
                queue.roundtrip(&mut state).unwrap();
                vk.key(now_ms(), code + 8, wl_keyboard::KeyState::Pressed);
                queue.roundtrip(&mut state).unwrap();
                vk.key(now_ms(), code + 8, wl_keyboard::KeyState::Released);
                vk.key(now_ms(), SHIFT_KEY + 8, wl_keyboard::KeyState::Released);
                vk.modifiers(now_ms(), 0, 0, 0, 0);
                queue.roundtrip(&mut state).unwrap();
            } else {
                for ch in tok.chars() {
                    let Some((code, shift)) = key_for(ch) else {
                        eprintln!("unknown char {ch:?}");
                        continue;
                    };
                    if shift {
                        vk.key(now_ms(), SHIFT_KEY + 8, wl_keyboard::KeyState::Pressed);
                        vk.modifiers(now_ms(), SHIFT_MOD, 0, 0, 0);
                        queue.roundtrip(&mut state).unwrap();
                    }
                    vk.key(now_ms(), code + 8, wl_keyboard::KeyState::Pressed);
                    queue.roundtrip(&mut state).unwrap();
                    vk.key(now_ms(), code + 8, wl_keyboard::KeyState::Released);
                    queue.roundtrip(&mut state).unwrap();
                    if shift {
                        vk.key(now_ms(), SHIFT_KEY + 8, wl_keyboard::KeyState::Released);
                        vk.modifiers(now_ms(), 0, 0, 0, 0);
                        queue.roundtrip(&mut state).unwrap();
                    }
                }
            }
        }
        return;
    }

    // pointer commands
    let manager = state.pointer_man.clone().expect("no zwlr_virtual_pointer_manager_v1");
    let pointer = manager.create_virtual_pointer(state.seat.as_ref(), &qh, ());
    queue.roundtrip(&mut state).unwrap();
    let cmd = &args[1..];
    match cmd {
        [op, "left" | "right"] => {
            let b = if cmd[1] == "left" { 0x110 } else { 0x111 };
            match op.as_str() {
                "press" => {
                    pointer.button(now_ms(), b, wl_pointer::ButtonState::Pressed);
                    pointer.frame();
                }
                "release" => {
                    pointer.button(now_ms(), b, wl_pointer::ButtonState::Released);
                    pointer.frame();
                }
                "click" => {
                    pointer.button(now_ms(), b, wl_pointer::ButtonState::Pressed);
                    pointer.frame();
                    pointer.button(now_ms(), b, wl_pointer::ButtonState::Released);
                    pointer.frame();
                }
                _ => usage(),
            }
        }
        ["move", x, y] => {
            let (ex, ey) = state.extents;
            pointer.motion_absolute(now_ms(), x.parse().unwrap(), y.parse().unwrap(), ex, ey);
        }
        ["scroll", dx, dy] => {
            let dx: i64 = dx.parse().unwrap();
            let dy: i64 = dy.parse().unwrap();
            if dy != 0 {
                pointer.axis(now_ms(), wl_pointer::Axis::VerticalScroll, dy as f64);
            }
            if dx != 0 {
                pointer.axis(now_ms(), wl_pointer::Axis::HorizontalScroll, dx as f64);
            }
            pointer.frame();
        }
        _ => usage(),
    }
    queue.roundtrip(&mut state).unwrap();
}

fn usage() -> ! {
    eprintln!("usage: vpointer move X Y | press|release|click B | scroll DX DY | type TOKENS...");
    std::process::exit(1);
}

fn tempfile_keymap() -> std::fs::File {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open("/tmp/pokey/vk.keymap.rs")
        .unwrap();
    f.write_all(KEYMAP.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}


