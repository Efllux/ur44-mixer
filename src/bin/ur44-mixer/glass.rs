//! Liquid Glass: the window background is a native AppKit glass view (NSGlassEffectView on
//! macOS 26, NSVisualEffectView before that) placed behind iced's drawing, and the panels and
//! controls are translucent so the glass shows through. macOS applies "Reduce transparency" and
//! light/dark adaptation to the native view itself.

use iced::widget::{button, container};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

/// Window settings for a glass window: transparent, with the content under a transparent title bar.
pub fn window_settings(glass: bool, size: iced::Size) -> iced::window::Settings {
    iced::window::Settings {
        size,
        exit_on_close_request: false,
        transparent: glass,
        // Without glass: a normal title bar (the content does not reserve space for the buttons).
        #[cfg(target_os = "macos")]
        platform_specific: iced::window::settings::PlatformSpecific {
            title_hidden: glass,
            titlebar_transparent: glass,
            fullsize_content_view: glass,
        },
        ..iced::window::Settings::default()
    }
}

/// Space at the top of the window for the traffic-light buttons (content runs under the title bar).
pub const TITLEBAR: f32 = 28.0;

/// Put the native glass view behind the window's content. Runs on the main thread (iced calls
/// `run_with_handle` callbacks from the event loop). Returns what was installed.
#[cfg(target_os = "macos")]
pub fn install(handle: iced::window::raw_window_handle::WindowHandle<'_>) -> &'static str {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2_app_kit::{
        NSAutoresizingMaskOptions, NSColor, NSGlassEffectView, NSGlassEffectViewStyle, NSView,
        NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView,
        NSWindowOrderingMode,
    };
    use iced::window::raw_window_handle::RawWindowHandle;

    let RawWindowHandle::AppKit(h) = handle.as_raw() else { return "no AppKit window" };
    let Some(mtm) = MainThreadMarker::new() else { return "not on the main thread" };
    // SAFETY: winit's NSView, alive for the duration of this callback; we are on the main thread.
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else { return "view has no window" };
    let Some(content) = window.contentView() else { return "window has no content view" };
    // The window's frame view hosts the content view; the glass goes below it, so it is behind
    // the Metal layer that iced draws into.
    let Some(frame_view) = (unsafe { content.superview() }) else { return "no frame view" };
    let bounds = frame_view.bounds();

    let effect: Retained<NSView> = if AnyClass::get(c"NSGlassEffectView").is_some() {
        let g = NSGlassEffectView::initWithFrame(mtm.alloc(), bounds);
        g.setStyle(NSGlassEffectViewStyle::Regular);
        // A dark tint keeps the light text readable over bright desktops.
        g.setTintColor(Some(&NSColor::colorWithSRGBRed_green_blue_alpha(0.06, 0.07, 0.10, 0.45)));
        Retained::into_super(g)
    } else {
        let v = NSVisualEffectView::initWithFrame(mtm.alloc(), bounds);
        v.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
        v.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        v.setState(NSVisualEffectState::FollowsWindowActiveState);
        Retained::into_super(v)
    };
    effect.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    frame_view.addSubview_positioned_relativeTo(&effect, NSWindowOrderingMode::Below, Some(&content));
    if AnyClass::get(c"NSGlassEffectView").is_some() { "NSGlassEffectView" } else { "NSVisualEffectView" }
}

#[cfg(not(target_os = "macos"))]
pub fn install(_: iced::window::raw_window_handle::WindowHandle<'_>) -> &'static str {
    "not macOS"
}

// ---- Translucent styles -------------------------------------------------------------------------

const RADIUS: f32 = 14.0;

fn white(a: f32) -> Color {
    Color { a, ..Color::WHITE }
}

/// The application background: transparent over glass, the theme colour without it.
pub fn background(glass: bool, theme: &Theme) -> Color {
    if glass { Color::TRANSPARENT } else { theme.palette().background }
}

/// A glass panel: faint fill, light top-lit edge, soft shadow.
pub fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(white(0.06))),
        border: Border { color: white(0.14), width: 1.0, radius: RADIUS.into() },
        shadow: Shadow { color: Color { a: 0.18, ..Color::BLACK }, offset: Vector::new(0.0, 4.0), blur_radius: 16.0 },
        ..container::Style::default()
    }
}

/// A smaller inset well (notices, meters' surroundings).
pub fn well(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color { a: 0.22, ..Color::BLACK })),
        border: Border { color: white(0.08), width: 1.0, radius: 10.0.into() },
        ..container::Style::default()
    }
}

/// Meter / gain-reduction trough.
pub fn trough(_: &Theme) -> container::Style {
    container::Style::default()
        .background(Color { a: 0.35, ..Color::BLACK })
        .border(iced::border::rounded(3))
}

fn capsule(bg: Color, fg: Color, edge: Color) -> button::Style {
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: fg,
        border: Border { color: edge, width: 1.0, radius: 999.0.into() },
        shadow: Shadow::default(),
    }
}

/// Unselected, translucent capsule.
pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let fg = theme.palette().text;
    match status {
        button::Status::Active => capsule(white(0.08), fg, white(0.14)),
        button::Status::Hovered => capsule(white(0.14), fg, white(0.22)),
        button::Status::Pressed => capsule(white(0.20), fg, white(0.26)),
        button::Status::Disabled => capsule(white(0.04), Color { a: 0.4, ..fg }, white(0.08)),
    }
}

/// A capsule tinted with `tint` (selected tab, accent, mute, solo, danger).
pub fn tinted(tint: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let a = match status {
            button::Status::Active => 0.70,
            button::Status::Hovered => 0.82,
            button::Status::Pressed => 0.92,
            button::Status::Disabled => 0.30,
        };
        capsule(Color { a, ..tint }, Color::WHITE, white(0.25))
    }
}

pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    tinted(theme.palette().primary)(theme, status)
}

pub fn danger(theme: &Theme, status: button::Status) -> button::Style {
    tinted(theme.palette().danger)(theme, status)
}

/// A pick list with the glass field and menu styles.
pub fn pick_list<'a, T, L, V, Message>(
    options: L,
    selected: Option<V>,
    on_selected: impl Fn(T) -> Message + 'a,
) -> iced::widget::PickList<'a, T, L, V, Message>
where
    T: ToString + PartialEq + Clone + 'a,
    L: std::borrow::Borrow<[T]> + 'a,
    V: std::borrow::Borrow<T> + 'a,
    Message: Clone,
{
    use iced::widget::{overlay::menu, pick_list as pl};
    iced::widget::pick_list(options, selected, on_selected)
        .style(|theme: &Theme, status| {
            let edge = match status {
                pl::Status::Active => white(0.14),
                pl::Status::Hovered | pl::Status::Opened => white(0.26),
            };
            pl::Style {
                text_color: theme.palette().text,
                placeholder_color: Color { a: 0.5, ..theme.palette().text },
                handle_color: theme.palette().text,
                background: Background::Color(white(0.08)),
                border: Border { color: edge, width: 1.0, radius: 10.0.into() },
            }
        })
        .menu_style(|theme: &Theme| menu::Style {
            // Menus float over other content: keep them nearly opaque for legibility.
            background: Background::Color(Color::from_rgba(0.10, 0.11, 0.15, 0.96)),
            border: Border { color: white(0.18), width: 1.0, radius: 10.0.into() },
            text_color: theme.palette().text,
            selected_text_color: Color::WHITE,
            selected_background: Background::Color(Color { a: 0.75, ..theme.palette().primary }),
        })
}

/// Text field on glass.
pub fn field(theme: &Theme, status: iced::widget::text_input::Status) -> iced::widget::text_input::Style {
    use iced::widget::text_input::{Status, Style};
    let edge = match status {
        Status::Focused => Color { a: 0.8, ..theme.palette().primary },
        Status::Hovered => white(0.26),
        _ => white(0.14),
    };
    Style {
        background: Background::Color(white(0.08)),
        border: Border { color: edge, width: 1.0, radius: 10.0.into() },
        icon: theme.palette().text,
        placeholder: Color { a: 0.5, ..theme.palette().text },
        value: theme.palette().text,
        selection: Color { a: 0.4, ..theme.palette().primary },
    }
}

/// Thin, translucent scroll bar.
pub fn scroll(_: &Theme, status: iced::widget::scrollable::Status) -> iced::widget::scrollable::Style {
    use iced::widget::scrollable::{Rail, Scroller, Status, Style};
    let a = match status {
        Status::Active => 0.18,
        Status::Hovered { .. } => 0.30,
        Status::Dragged { .. } => 0.42,
    };
    let rail = Rail {
        background: None,
        border: Border::default(),
        scroller: Scroller { color: white(a), border: Border { radius: 999.0.into(), ..Border::default() } },
    };
    Style { container: container::Style::default(), vertical_rail: rail, horizontal_rail: rail, gap: None }
}

// ---- State, mix and fader styles ----------------------------------------------------------------

/// Tint of each monitor mix: MIX 1 blue, MIX 2 magenta, so the mix being edited is obvious.
pub fn mix_tint(mix: u8) -> Color {
    if mix == 2 { Color::from_rgb(0.90, 0.46, 0.84) } else { Color::from_rgb(0.38, 0.60, 1.0) }
}

/// Text hierarchy: headings and values, ordinary labels, secondary explanations.
pub const TEXT_HI: Color = Color { r: 0.96, g: 0.97, b: 0.99, a: 1.0 };
pub const TEXT: Color = Color { r: 0.86, g: 0.88, b: 0.92, a: 1.0 };
pub const TEXT_MUTED: Color = Color { r: 0.62, g: 0.65, b: 0.73, a: 1.0 };

/// The app theme: Tokyo Night Storm colours with neutral (not lavender) text.
pub fn app_theme() -> Theme {
    static THEME: std::sync::LazyLock<Theme> = std::sync::LazyLock::new(|| {
        let mut p = iced::theme::Palette::TOKYO_NIGHT_STORM;
        p.text = TEXT;
        Theme::custom("UR44".into(), p)
    });
    THEME.clone()
}

/// Neutral inset behind settings that are shared by both mixes (input setup, reverb sends).
pub fn shared_inset(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color { a: 0.20, ..Color::BLACK })),
        border: Border { color: white(0.07), width: 1.0, radius: 9.0.into() },
        ..container::Style::default()
    }
}

/// Slider for a shared setting (reverb send): neutral grey, hollow handle.
pub fn slider_shared(_: &Theme, status: iced::widget::slider::Status) -> iced::widget::slider::Style {
    use iced::widget::slider::{Handle, HandleShape, Rail, Status, Style};
    let lift = if matches!(status, Status::Active) { 0.0 } else { 0.08 };
    Style {
        rail: Rail {
            backgrounds: (Background::Color(white(0.62 + lift)), Background::Color(white(0.14))),
            width: 3.0,
            border: Border { radius: 2.0.into(), ..Border::default() },
        },
        handle: Handle {
            shape: HandleShape::Circle { radius: 6.0 },
            background: Background::Color(Color::from_rgb(0.16, 0.17, 0.22)),
            border_width: 2.0,
            border_color: white(0.85 + lift),
        },
    }
}

/// Slider for a per-mix setting (pan, balance): in the mix tint.
pub fn slider_mix(mix: u8) -> impl Fn(&Theme, iced::widget::slider::Status) -> iced::widget::slider::Style {
    use iced::widget::slider::{Handle, HandleShape, Rail, Status, Style};
    move |_, status| {
        let t = mix_tint(mix);
        let lift = if matches!(status, Status::Active) { 0.0 } else { 0.1 };
        Style {
            rail: Rail {
                backgrounds: (Background::Color(Color { a: 0.75, ..t }), Background::Color(white(0.14))),
                width: 3.0,
                border: Border { radius: 2.0.into(), ..Border::default() },
            },
            handle: Handle {
                shape: HandleShape::Circle { radius: 6.0 },
                background: Background::Color(Color { r: (t.r + lift).min(1.0), g: (t.g + lift).min(1.0), b: (t.b + lift).min(1.0), a: 1.0 }),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
        }
    }
}

/// Clickable input name: plain text that lights up when its inspector is open.
pub fn name_button(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let bg = match (selected, status) {
            (true, _) => white(0.16),
            (false, button::Status::Hovered | button::Status::Pressed) => white(0.10),
            _ => Color::TRANSPARENT,
        };
        let edge = if selected { white(0.30) } else { Color::TRANSPARENT };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: TEXT_HI,
            border: Border { color: edge, width: 1.0, radius: 7.0.into() },
            shadow: Shadow::default(),
        }
    }
}

/// Warm warning colour for phantom power.
pub const PHANTOM: Color = Color { r: 0.96, g: 0.42, b: 0.26, a: 1.0 };
/// Connection-status colours.
pub const OK: Color = Color { r: 0.38, g: 0.86, b: 0.52, a: 1.0 };
pub const WAIT: Color = Color { r: 0.98, g: 0.74, b: 0.30, a: 1.0 };
pub const OFF: Color = Color { r: 0.94, g: 0.40, b: 0.42, a: 1.0 };

/// A toggle that is off: outlined, no fill, dimmer label.
pub fn toggle_off(theme: &Theme, status: button::Status) -> button::Style {
    let fg = theme.palette().text;
    match status {
        button::Status::Active => capsule(white(0.015), Color { a: 0.78, ..fg }, white(0.26)),
        button::Status::Hovered => capsule(white(0.07), fg, white(0.40)),
        button::Status::Pressed => capsule(white(0.12), fg, white(0.45)),
        button::Status::Disabled => capsule(Color::TRANSPARENT, Color { a: 0.35, ..fg }, white(0.10)),
    }
}

/// A toggle that is on: solid fill in `tint`, white label, bright edge.
pub fn toggle_on(tint: Color) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let k = match status {
            button::Status::Active => 1.0,
            button::Status::Hovered => 1.08,
            button::Status::Pressed => 0.9,
            button::Status::Disabled => 0.5,
        };
        let c = Color { r: (tint.r * 0.82 * k).min(1.0), g: (tint.g * 0.82 * k).min(1.0), b: (tint.b * 0.82 * k).min(1.0), a: 1.0 };
        let edge = Color { r: (tint.r * 1.15).min(1.0), g: (tint.g * 1.15).min(1.0), b: (tint.b * 1.15).min(1.0), a: 1.0 };
        // Dark label on light fills (solo yellow), white otherwise.
        let lum = 0.299 * c.r + 0.587 * c.g + 0.114 * c.b;
        let fg = if lum > 0.62 { Color::from_rgb(0.10, 0.10, 0.12) } else { Color::WHITE };
        let mut s = capsule(c, fg, edge);
        s.shadow = Shadow { color: Color { a: 0.45, ..tint }, offset: Vector::new(0.0, 0.0), blur_radius: 8.0 };
        s
    }
}

/// Navigation tab: the selected page is a bright neutral chip (distinct from "on" toggles).
pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        if active {
            capsule(Color::from_rgb(0.86, 0.88, 0.94), Color::from_rgb(0.10, 0.11, 0.16), white(0.9))
        } else {
            let mut s = secondary(theme, status);
            s.background = Some(Background::Color(match status {
                button::Status::Hovered | button::Status::Pressed => white(0.12),
                _ => Color::TRANSPARENT,
            }));
            s.border.color = Color::TRANSPARENT;
            s
        }
    }
}

/// One half of the mix selector: the selected mix is filled with its tint.
pub fn mix_choice(mix: u8, selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| if selected { toggle_on(mix_tint(mix))(theme, status) } else { toggle_off(theme, status) }
}

/// Header band of the mix being edited, tinted with the mix colour.
pub fn mix_band(mix: u8) -> impl Fn(&Theme) -> container::Style {
    move |_| {
        let t = mix_tint(mix);
        container::Style {
            background: Some(Background::Color(Color { a: 0.13, ..t })),
            border: Border { color: Color { a: 0.55, ..t }, width: 1.0, radius: 12.0.into() },
            ..container::Style::default()
        }
    }
}

/// A mixer strip, edged with the mix tint; the input shown in the inspector gets a bright edge.
pub fn strip_panel(mix: u8, selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = mix_tint(mix);
        let mut s = panel(theme);
        s.background = Some(Background::Color(Color { a: if selected { 0.14 } else { 0.08 }, ..t }));
        s.border = if selected {
            Border { color: white(0.70), width: 2.0, radius: RADIUS.into() }
        } else {
            Border { color: Color { a: 0.22, ..t }, width: 1.0, radius: RADIUS.into() }
        };
        s
    }
}

/// Status badge (+48V): lit = warm filled with white text, off = outlined and dim.
pub fn badge(lit: bool, tint: Color) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let (bg, fg, edge) = if lit {
            (Color { a: 0.92, ..tint }, Color::WHITE, tint)
        } else {
            (Color::TRANSPARENT, Color { a: 0.62, ..theme.palette().text }, white(0.20))
        };
        container::Style {
            background: Some(Background::Color(bg)),
            text_color: Some(fg),
            border: Border { color: edge, width: 1.0, radius: 999.0.into() },
            shadow: if lit { Shadow { color: Color { a: 0.5, ..tint }, offset: Vector::new(0.0, 0.0), blur_radius: 8.0 } } else { Shadow::default() },
            ..container::Style::default()
        }
    }
}

/// Fader cap height (the slider handle) in pixels; graphs::FaderScale uses the same travel.
pub const CAP_H: u16 = 30;

/// Mixer fader: a wide rail lit in the mix tint below the cap, and a rectangular cap with a
/// centre line marking the exact position.
pub fn fader(mix: u8) -> impl Fn(&Theme, iced::widget::slider::Status) -> iced::widget::slider::Style {
    use iced::widget::slider::{Handle, HandleShape, Rail, Status, Style};
    use iced::gradient::{Gradient, Linear};
    move |_, status| {
        let t = mix_tint(mix);
        let lift = match status {
            Status::Active => 0.0,
            Status::Hovered => 0.04,
            Status::Dragged => 0.08,
        };
        let body = Color::from_rgb(0.80 + lift, 0.82 + lift, 0.87 + lift);
        let shade = Color::from_rgb(0.58 + lift, 0.60 + lift, 0.66 + lift);
        let cap = Gradient::Linear(
            Linear::new(iced::Radians(0.0))
                .add_stop(0.0, shade)
                .add_stop(0.40, body)
                .add_stop(0.44, t)
                .add_stop(0.56, t)
                .add_stop(0.60, body)
                .add_stop(1.0, shade),
        );
        Style {
            rail: Rail {
                backgrounds: (Background::Color(Color { a: 0.50, ..t }), Background::Color(Color { a: 0.55, ..Color::BLACK })),
                width: 6.0,
                border: Border { radius: 3.0.into(), ..Border::default() },
            },
            handle: Handle {
                shape: HandleShape::Rectangle { width: CAP_H, border_radius: 4.0.into() },
                background: Background::Gradient(cap),
                border_width: 1.0,
                border_color: Color::from_rgba(0.0, 0.0, 0.0, 0.55),
            },
        }
    }
}
