//! The React Bits `Stepper` onboarding demo ported to GPUI, following comparison/stepper/SPEC.md.
//!
//! `cargo run --release -- [--step N] [--name NAME] [--plan pro|starter] [--focus] [--capture out.png]`
mod input;
mod motion;

use gpui::{
    Animation, AnimationExt, AnyElement, App, Application, Background, Bounds, BoxShadow, Context,
    Div, ElementId, Entity, Focusable, FontWeight, PathBuilder, PathStyle, Pixels, Rgba,
    StrokeOptions, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div,
    linear_color_stop, linear_gradient, point, prelude::*, px, relative, size,
};
use input::TextInput;
use lyon::tessellation::{LineCap, LineJoin};
use motion::{Transition, cubic_bezier, ease, ease_out, lerp, mix, spring};
use std::{cell::RefCell, path::PathBuf, process::Command, rc::Rc, time::Duration};

// Dark theme of Stepper.jsx.
const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Rgba {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a,
    }
}
const PAGE: Rgba = rgba(0x0b, 0x0b, 0x0f, 1.);
const FRAME: Rgba = rgba(38, 35, 46, 0.66);
const FRAME_EDGE: Rgba = rgba(255, 255, 255, 0.08);
const FRAME_HIGHLIGHT: Rgba = rgba(255, 255, 255, 0.07);
const TILE_TOP: Rgba = rgba(0x3a, 0x36, 0x44, 1.);
const TILE_BOTTOM: Rgba = rgba(0x27, 0x24, 0x2f, 1.);
const TILE_EDGE: Rgba = rgba(255, 255, 255, 0.07);
const TILE_HIGHLIGHT: Rgba = rgba(255, 255, 255, 0.12);
const WELL: Rgba = rgba(255, 255, 255, 0.07);
const INK: Rgba = rgba(0xf4, 0xf4, 0xf5, 1.);
const MUTED: Rgba = rgba(244, 244, 245, 0.55);
const FAINT: Rgba = rgba(255, 255, 255, 0.22);
const RING: Rgba = rgba(244, 244, 245, 0.3);
const SOLID: Rgba = INK;
const SOLID_INK: Rgba = rgba(0x18, 0x18, 0x1b, 1.);
const HOVER_FILL: Rgba = rgba(255, 255, 255, 0.04);
// Chromium's default `::placeholder` color.
// Chromium's default `::placeholder` color, measured in the reference capture.
const PLACEHOLDER: Rgba = rgba(0x75, 0x75, 0x75, 1.);
/// CSS `line-height: normal` for Inter: (ascender + descender) / units per em.
const NORMAL: f32 = 1.2102;
const TITLES: [&str; 4] = ["Welcome", "Plan", "Profile", "Review"];
/// Footer padding-top 20 + button height 38.
const FOOTER_HEIGHT: f32 = 58.;
const SEMIBOLD: FontWeight = FontWeight::SEMIBOLD;

fn clear(color: Rgba) -> Rgba {
    Rgba { a: 0., ..color }
}

/// CSS `filter: brightness(k)` on a color.
fn bright(c: Rgba, k: f32) -> Rgba {
    Rgba {
        r: (c.r * k).min(1.),
        g: (c.g * k).min(1.),
        b: (c.b * k).min(1.),
        a: c.a,
    }
}

/// A CSS box-shadow. GPUI's blur radius is the Gaussian sigma, half of the CSS blur.
fn shadow(y: f32, blur: f32, spread: f32, alpha: f32) -> BoxShadow {
    BoxShadow {
        color: rgba(0, 0, 0, alpha).into(),
        offset: point(px(0.), px(y)),
        blur_radius: px(blur / 2.),
        spread_radius: px(spread),
    }
}

fn tile(k: f32) -> Background {
    linear_gradient(
        180.,
        linear_color_stop(bright(TILE_TOP, k), 0.),
        linear_color_stop(bright(TILE_BOTTOM, k), 1.),
    )
}

/// CSS `inset 0 0 0 1px edge, inset 0 1px 0 highlight`, drawn over the box without taking layout space.
fn rims(radius: f32, edge: Rgba, highlight: Rgba) -> [Div; 2] {
    let overlay = || div().absolute().inset_0().rounded(px(radius));
    [
        overlay().border_t_1().border_color(highlight),
        overlay().border_1().border_color(edge),
    ]
}

fn heading(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .text_size(px(17.))
        .font_weight(SEMIBOLD)
        .line_height(relative(1.3))
        .child(text.into())
}

fn copy(text: impl Into<gpui::SharedString>) -> Div {
    div()
        .mt(px(6.))
        .text_size(px(13.5))
        .line_height(relative(1.55))
        .text_color(MUTED)
        .child(text.into())
}

fn id(name: &'static str, n: usize) -> ElementId {
    ElementId::NamedInteger(name.into(), n as u64)
}

/// Runs `apply` over `ms` with `easing` while `generation` names a change; generation 0 means the value never
/// changed, so it is drawn at its end state without animating (as React does on first render).
fn animate<E: IntoElement + 'static>(
    element: E,
    id: ElementId,
    generation: usize,
    ms: u64,
    easing: impl Fn(f32) -> f32 + 'static,
    apply: impl Fn(E, f32) -> E + 'static,
) -> AnyElement {
    if generation == 0 {
        apply(element, 1.).into_any_element()
    } else {
        element
            .with_animation(
                id,
                Animation::new(Duration::from_millis(ms)).with_easing(easing),
                apply,
            )
            .into_any_element()
    }
}

#[derive(Clone, Copy)]
enum Icon {
    ArrowLeft,
    ArrowRight,
    Tick,
    CheckCircle,
}

/// A Hugeicons stroke icon: 24 × 24 viewBox, round caps and joins. `dash` keeps only that much of the stroke,
/// in viewBox units, for the check mark's draw-on animation.
fn icon(icon: Icon, size: f32, stroke: f32, color: Rgba, dash: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            if dash.is_some_and(|d| d < 0.01) {
                return;
            }
            let s = size / 24.;
            let p =
                |x: f32, y: f32| point(bounds.origin.x + px(x * s), bounds.origin.y + px(y * s));
            let options = StrokeOptions::default()
                .with_line_width(stroke * s)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round);
            let mut path =
                PathBuilder::stroke(px(stroke * s)).with_style(PathStyle::Stroke(options));
            if let Some(d) = dash {
                path = path.dash_array(&[px(d * s), px(100.)]);
            }
            match icon {
                Icon::ArrowLeft => {
                    path.move_to(p(15., 6.));
                    path.cubic_bezier_to(p(9., 12.), p(15., 6.), p(9., 10.4189));
                    path.cubic_bezier_to(p(15., 18.), p(9., 13.5812), p(15., 18.));
                }
                Icon::ArrowRight => {
                    path.move_to(p(9., 6.));
                    path.cubic_bezier_to(p(15., 12.), p(9., 6.), p(15., 10.4189));
                    path.cubic_bezier_to(p(9., 18.), p(15., 13.5812), p(9., 18.));
                }
                Icon::Tick => {
                    path.move_to(p(5., 14.));
                    path.line_to(p(8.5, 17.5));
                    path.line_to(p(19., 6.5));
                }
                Icon::CheckCircle => {
                    path.move_to(p(22., 12.));
                    path.cubic_bezier_to(p(12., 2.), p(22., 6.47715), p(17.5228, 2.));
                    path.cubic_bezier_to(p(2., 12.), p(6.47715, 2.), p(2., 6.47715));
                    path.cubic_bezier_to(p(12., 22.), p(2., 17.5228), p(6.47715, 22.));
                    path.cubic_bezier_to(p(22., 12.), p(17.5228, 22.), p(22., 17.5228));
                    path.close();
                    path.move_to(p(8., 12.5));
                    path.line_to(p(10.5, 15.));
                    path.line_to(p(16., 9.));
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .size(px(size))
}

#[derive(Clone, Copy, PartialEq)]
enum Plan {
    Starter,
    Pro,
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Complete,
    Active,
    Upcoming,
}

fn status(number: usize, current: usize) -> Status {
    if current > TITLES.len() || number < current {
        Status::Complete
    } else if number == current {
        Status::Active
    } else {
        Status::Upcoming
    }
}

/// Bounds measured after layout. Animations read them on the next frames, like the original reads offsets.
#[derive(Default)]
struct Measured {
    /// Segment bounds relative to the glider's layer, which covers the track.
    segments: [Bounds<Pixels>; 4],
    pane: f32,
    /// What the glider and the viewport show now, so an interrupted animation restarts from there.
    glider: Option<Bounds<Pixels>>,
    viewport: Option<f32>,
}

/// Records an element's bounds after layout and asks for another frame when they change.
fn measure(
    measured: &Rc<RefCell<Measured>>,
    write: impl Fn(&mut Measured, Bounds<Pixels>) -> bool + 'static,
) -> impl IntoElement {
    let measured = measured.clone();
    canvas(
        move |bounds, window, _| {
            if write(&mut measured.borrow_mut(), bounds) {
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

struct Stepper {
    current: usize,
    /// Step shown before the last change: the source of every step-driven transition.
    from_step: usize,
    leaving: Option<usize>,
    direction: f32,
    generation: usize,
    glider_from: Option<Bounds<Pixels>>,
    viewport_from: Option<f32>,
    plan: Transition<Plan>,
    field_focus: Transition<bool>,
    next_disabled: Transition<bool>,
    back_hidden: Transition<bool>,
    completed: Transition<bool>,
    back_hovered: bool,
    next_hovered: bool,
    back_hover: Transition<bool>,
    next_hover: Transition<bool>,
    name: Entity<TextInput>,
    measured: Rc<RefCell<Measured>>,
}

impl Stepper {
    fn go(&mut self, target: usize, cx: &mut Context<Self>) {
        let next = target.clamp(1, TITLES.len() + 1);
        if next == self.current {
            return;
        }
        let measured = self.measured.borrow();
        self.glider_from = measured.glider;
        self.viewport_from = measured.viewport.or(Some(measured.pane));
        drop(measured);
        self.direction = if next > self.current { 1. } else { -1. };
        (self.leaving, self.from_step, self.current) = (Some(self.current), self.current, next);
        self.generation += 1;
        let generation = self.generation;
        // The leaving pane is dropped when its 200 ms exit ends, unless another change replaced it.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.leaving = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn name(&self, cx: &App) -> String {
        self.name.read(cx).content.trim().to_owned()
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let generation = self.generation;
        let target = self.current.min(TITLES.len()) - 1;
        let from = self.glider_from;
        let measured = self.measured.clone();
        let glider = div()
            .absolute()
            .rounded(px(12.))
            .bg(tile(1.))
            .shadow(vec![shadow(4., 10., -4., 0.7)]);
        let glider = animate(
            glider,
            id("glider", generation),
            generation,
            560,
            spring,
            move |el, t| {
                let mut m = measured.borrow_mut();
                let to = m.segments[target];
                let b = from.map_or(to, |from| {
                    let at = |a: Pixels, b: Pixels| px(lerp(a.into(), b.into(), t));
                    Bounds::new(
                        point(
                            at(from.origin.x, to.origin.x),
                            at(from.origin.y, to.origin.y),
                        ),
                        size(
                            at(from.size.width, to.size.width),
                            at(from.size.height, to.size.height),
                        ),
                    )
                });
                m.glider = Some(b);
                el.left(b.origin.x)
                    .top(b.origin.y)
                    .w(b.size.width)
                    .h(b.size.height)
                    .children(rims(12., TILE_EDGE, TILE_HIGHLIGHT))
            },
        );
        let visible = self.completed;
        let glider = animate(
            div().absolute().inset_0().child(glider),
            id("glider-fade", visible.generation),
            visible.generation,
            200,
            ease(),
            move |el, t| {
                el.opacity(lerp(
                    if visible.from { 0. } else { 1. },
                    if visible.to { 0. } else { 1. },
                    t,
                ))
            },
        );
        let segments = (1..=TITLES.len()).map(|number| {
            let (from, to) = (status(number, self.from_step), status(number, self.current));
            let segment = div()
                .id(id("segment", number))
                .relative()
                .flex()
                .flex_auto()
                .min_w_0()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .h(px(40.))
                .px(px(10.))
                .rounded(px(12.))
                .child(badge(number, from, to, generation))
                .child(animate(
                    div()
                        .min_w_0()
                        .text_size(px(13.))
                        .font_weight(SEMIBOLD)
                        .line_height(relative(1.2))
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(TITLES[number - 1]),
                    id("title", number * 100_000 + generation),
                    generation,
                    200,
                    ease(),
                    move |el, t| {
                        let color = |s| if s == Status::Upcoming { MUTED } else { INK };
                        el.text_color(mix(color(from), color(to), t))
                    },
                ));
            if number == self.current {
                segment
            } else {
                segment
                    .cursor_pointer()
                    .hover(|s| s.bg(HOVER_FILL))
                    .on_click(cx.listener(move |this, _, _, cx| this.go(number, cx)))
            }
        });
        let measured = self.measured.clone();
        div()
            .relative()
            .flex()
            .gap(px(4.))
            .p(px(4.))
            .rounded(px(16.))
            .bg(WELL)
            // Children: the glider's layer, then the segments. Like the original's offsetLeft/offsetWidth.
            .on_children_prepainted(move |bounds, window, _| {
                let mut m = measured.borrow_mut();
                let origin = bounds[0].origin;
                let segments: Vec<_> = bounds[1..]
                    .iter()
                    .map(|b| Bounds::new(b.origin - origin, b.size))
                    .collect();
                if m.segments[..] != segments[..] {
                    m.segments.copy_from_slice(&segments);
                    window.request_animation_frame();
                }
            })
            .child(glider)
            .children(segments)
    }

    fn pane(&self, step: usize, cx: &mut Context<Self>) -> Div {
        div()
            .min_w_0()
            .px(px(20.))
            .py(px(6.))
            .child(self.content(step, cx))
    }

    fn content(&self, step: usize, cx: &mut Context<Self>) -> AnyElement {
        let name = self.name(cx);
        let suffix = if name.is_empty() {
            String::new()
        } else {
            format!(", {name}")
        };
        match step {
            1 => div()
                .child(heading("Welcome aboard"))
                .child(copy(
                    "Set up your workspace in a few quick steps. It only takes a minute.",
                ))
                .into_any_element(),
            2 => div()
                .child(heading("Choose a plan"))
                .child(copy("You can change this at any time."))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .mt(px(14.))
                        .child(self.option(Plan::Starter, "Starter", "Free forever", cx))
                        .child(self.option(Plan::Pro, "Pro", "$12 per month", cx)),
                )
                .into_any_element(),
            3 => div()
                .child(heading("What should we call you?"))
                .child(self.field())
                .into_any_element(),
            4 => {
                let plan = if self.plan.to == Plan::Pro {
                    "Pro"
                } else {
                    "Starter"
                };
                div()
                    .child(heading(format!("Looking good{suffix}")))
                    .child(copy(format!(
                        "You picked the {plan} plan. Complete setup to open your workspace."
                    )))
                    .into_any_element()
            }
            _ => div()
                .flex()
                .items_center()
                .gap(px(14.))
                .py(px(4.))
                .child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(40.))
                        .rounded(px(12.))
                        .bg(WELL)
                        .child(icon(Icon::CheckCircle, 20., 1.8, INK, None)),
                )
                .child(
                    div()
                        .child(heading("Workspace ready"))
                        .child(copy(format!("Welcome in{suffix}.")).mt(px(2.))),
                )
                .into_any_element(),
        }
    }

    fn option(
        &self,
        plan: Plan,
        title: &'static str,
        detail: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = self.plan;
        let (from, to) = (t.from == plan, t.to == plan);
        let element = div()
            .id(id("option", plan as usize))
            .relative()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .py(px(11.))
            .px(px(14.))
            .rounded(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.plan.set(plan);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .min_w_0()
                    .child(div().text_size(px(14.)).font_weight(SEMIBOLD).child(title))
                    .child(div().text_size(px(12.5)).text_color(MUTED).child(detail)),
            );
        // The tile gradient is a background image, so it switches at once; the color under it fades.
        animate(
            element,
            id("option-look", plan as usize * 100_000 + t.generation),
            t.generation,
            200,
            ease(),
            move |el, t| {
                let selected = |s: bool| if s { 1. } else { 0. };
                let s = lerp(selected(from), selected(to), t);
                let el = if to {
                    el.bg(tile(1.))
                } else {
                    el.bg(mix(clear(WELL), WELL, 1. - s))
                };
                el.shadow(vec![shadow(4., 10., -4., 0.7 * s)]).child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(mix(TILE_EDGE, RING, s)),
                )
            },
        )
    }

    fn field(&self) -> AnyElement {
        let focus = self.field_focus;
        let element = div()
            .relative()
            .mt(px(14.))
            .h(px(42.))
            .px(px(14.))
            .rounded(px(12.))
            .bg(WELL)
            .text_size(px(14.))
            .child(self.name.clone());
        animate(
            element,
            id("field", focus.generation),
            focus.generation,
            200,
            ease(),
            move |el, t| {
                let ring = |focused: bool| if focused { RING } else { TILE_EDGE };
                el.child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(mix(ring(focus.from), ring(focus.to), t)),
                )
            },
        )
    }

    fn viewport(&self, cx: &mut Context<Self>) -> AnyElement {
        let generation = self.generation;
        let direction = self.direction;
        let mut viewport = div().relative().mx(px(-20.)).my(px(-6.)).overflow_hidden();
        if let Some(leaving) = self.leaving {
            let pane = animate(
                self.pane(leaving, cx),
                id("leave-fade", generation),
                generation,
                140,
                cubic_bezier(0.2, 0., 0.4, 1.),
                |el, p| el.opacity(1. - p),
            );
            viewport = viewport.child(animate(
                div().absolute().top_0().child(pane),
                id("leave-slide", generation),
                generation,
                200,
                cubic_bezier(0.4, 0., 1., 1.),
                move |el, p| {
                    el.left(px(-direction * 36. * p))
                        .right(px(direction * 36. * p))
                },
            ));
        }
        let pane = self
            .pane(self.current, cx)
            .relative()
            .child(measure(&self.measured, |m, b| {
                std::mem::replace(&mut m.pane, b.size.height.into()) != f32::from(b.size.height)
            }));
        let pane = animate(
            pane,
            id("pane-fade", generation),
            generation,
            420,
            ease_out(),
            |el, p| el.opacity(if p < 0.25 { 0. } else { (p - 0.25) / 0.75 }),
        );
        let pane = animate(
            div().relative().child(pane),
            id("pane-slide", generation),
            generation,
            560,
            spring,
            move |el, t| el.left(px(direction * 36. * (1. - t))),
        );
        let viewport = viewport.child(pane);
        if generation == 0 {
            return viewport.into_any_element();
        }
        let (measured, from) = (self.measured.clone(), self.viewport_from);
        animate(
            viewport,
            id("viewport", generation),
            generation,
            560,
            spring,
            move |el, t| {
                let mut m = measured.borrow_mut();
                let height = lerp(from.unwrap_or(m.pane), m.pane, t);
                m.viewport = Some(height);
                el.h(px(height))
            },
        )
    }

    fn back_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let (hover, hidden) = (self.back_hover, self.back_hidden);
        let mut button = div()
            .id("back")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(38.))
            .pl(px(10.))
            .pr(px(14.))
            .rounded(px(12.))
            .text_size(px(13.5))
            .font_weight(SEMIBOLD)
            .line_height(relative(1.))
            .shadow(vec![shadow(4., 10., -4., 0.7)]);
        if !hidden.to && !self.completed.to {
            button = button
                .cursor_pointer()
                .active(|s| s.bg(tile(0.85)))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.back_hovered = *hovered;
                    cx.notify();
                }))
                .on_click(cx.listener(|this, _, _, cx| this.go(this.current - 1, cx)));
        }
        let button = animate(
            button,
            id("back-hover", hover.generation),
            hover.generation,
            200,
            ease(),
            move |el, t| {
                let k = lerp(
                    if hover.from { 1.12 } else { 1. },
                    if hover.to { 1.12 } else { 1. },
                    t,
                );
                el.bg(tile(k))
                    .children(rims(12., bright(TILE_EDGE, k), bright(TILE_HIGHLIGHT, k)))
                    .text_color(bright(INK, k))
                    .child(icon(Icon::ArrowLeft, 16., 2., bright(INK, k), None))
                    .child("Back")
            },
        );
        animate(
            div().child(button),
            id("back-hidden", hidden.generation),
            hidden.generation,
            200,
            ease(),
            move |el, t| {
                el.opacity(lerp(
                    if hidden.from { 0. } else { 1. },
                    if hidden.to { 0. } else { 1. },
                    t,
                ))
            },
        )
    }

    fn next_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let (hover, disabled) = (self.next_hover, self.next_disabled);
        let last = self.current == TITLES.len();
        let mut button = div()
            .id("next")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(38.))
            .pl(px(14.))
            .pr(px(11.))
            .rounded(px(12.))
            .text_size(px(13.5))
            .font_weight(SEMIBOLD)
            .line_height(relative(1.))
            .shadow(vec![shadow(4., 12., -6., 0.5)]);
        if !disabled.to && !self.completed.to {
            button = button
                .cursor_pointer()
                .active(|s| s.bg(bright(SOLID, 0.85)))
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.next_hovered = *hovered;
                    cx.notify();
                }))
                .on_click(cx.listener(|this, _, _, cx| this.go(this.current + 1, cx)));
        }
        let button = animate(
            button,
            id("next-hover", hover.generation),
            hover.generation,
            200,
            ease(),
            move |el, t| {
                let k = lerp(
                    if hover.from { 0.9 } else { 1. },
                    if hover.to { 0.9 } else { 1. },
                    t,
                );
                el.bg(bright(SOLID, k))
                    .text_color(bright(SOLID_INK, k))
                    .child(if last { "Complete" } else { "Continue" })
                    .child(icon(
                        if last { Icon::Tick } else { Icon::ArrowRight },
                        16.,
                        2.,
                        bright(SOLID_INK, k),
                        None,
                    ))
            },
        );
        // `justify_between` alone: Taffy shifts an auto margin combined with `gap` by one gap.
        animate(
            div().child(button),
            id("next-disabled", disabled.generation),
            disabled.generation,
            200,
            ease(),
            move |el, t| {
                el.opacity(lerp(
                    if disabled.from { 0.4 } else { 1. },
                    if disabled.to { 0.4 } else { 1. },
                    t,
                ))
            },
        )
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let hidden = self.completed;
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .pt(px(20.))
            .child(self.back_button(cx))
            .child(self.next_button(cx));
        // Clipped only while hidden, like `.stepper__footer-wrap[data-hidden]`, so the button shadow shows otherwise.
        let wrap = div()
            .when(hidden.to, |el| el.overflow_hidden())
            .child(footer);
        let height = |h: bool| if h { 0. } else { FOOTER_HEIGHT };
        let wrap = animate(
            wrap,
            id("footer-height", hidden.generation),
            hidden.generation,
            480,
            spring,
            move |el, t| {
                if hidden.generation == 0 && !hidden.to {
                    el
                } else {
                    el.h(px(lerp(height(hidden.from), height(hidden.to), t)))
                }
            },
        );
        animate(
            div().child(wrap),
            id("footer-fade", hidden.generation),
            hidden.generation,
            200,
            ease(),
            move |el, t| {
                el.opacity(lerp(
                    if hidden.from { 0. } else { 1. },
                    if hidden.to { 0. } else { 1. },
                    t,
                ))
            },
        )
    }
}

fn badge(number: usize, from: Status, to: Status, generation: usize) -> AnyElement {
    let content = if to == Status::Complete {
        // Mounted when the step completes; the stroke draws on over 360 ms like the original `Check`.
        div()
            .size(px(16.))
            .with_animation(
                id("check", number),
                Animation::new(Duration::from_millis(360))
                    .with_easing(cubic_bezier(0.22, 1., 0.36, 1.)),
                |el, t| el.child(icon(Icon::Tick, 16., 2.4, SOLID_INK, Some(24. * t))),
            )
            .into_any_element()
    } else {
        number.to_string().into_any_element()
    };
    let element = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded_full()
        .text_size(px(11.5))
        .font_weight(SEMIBOLD)
        .line_height(relative(1.))
        .child(content);
    animate(
        element,
        id("badge", number * 100_000 + generation),
        generation,
        150,
        ease(),
        move |el, t| {
            // (background, ring, ring width, text)
            let look = |s| match s {
                Status::Complete => (SOLID, clear(INK), 0., SOLID_INK),
                Status::Active => (clear(SOLID), INK, 1.5, INK),
                Status::Upcoming => (clear(SOLID), FAINT, 1., MUTED),
            };
            let (a, b) = (look(from), look(to));
            el.bg(mix(a.0, b.0, t))
                .border(px(lerp(a.2, b.2, t)))
                .border_color(mix(a.1, b.1, t))
                .text_color(mix(a.3, b.3, t))
        },
    )
}

impl Render for Stepper {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.name(cx).is_empty();
        self.field_focus
            .set(self.name.focus_handle(cx).is_focused(window));
        self.next_disabled.set(self.current == 3 && empty);
        self.back_hidden.set(self.current <= 1);
        self.completed.set(self.current > TITLES.len());
        self.back_hover
            .set(self.back_hovered && !self.back_hidden.to);
        self.next_hover
            .set(self.next_hovered && !self.next_disabled.to);
        let body = div()
            .flex()
            .flex_col()
            .min_w_0()
            .pt(px(20.))
            .px(px(12.))
            .pb(px(12.))
            .child(self.viewport(cx))
            .child(self.footer(cx));
        div()
            .size_full()
            .bg(PAGE)
            .font_family("Inter")
            .text_color(INK)
            .line_height(relative(NORMAL))
            .child(
                div()
                    .absolute()
                    .left(px(100.))
                    .top(px(48.))
                    .w(px(440.))
                    .flex()
                    .flex_col()
                    .rounded(px(24.))
                    .p(px(8.))
                    .bg(FRAME)
                    .shadow(vec![shadow(22., 44., -20., 0.8), shadow(6., 16., -8., 0.5)])
                    .child(self.header(cx))
                    .child(body)
                    .children(rims(24., FRAME_EDGE, FRAME_HIGHLIGHT)),
            )
    }
}

struct Args {
    step: usize,
    name: String,
    plan: Plan,
    focus: bool,
    capture: Option<PathBuf>,
}

fn args() -> Args {
    let mut args = Args {
        step: 1,
        name: String::new(),
        plan: Plan::Pro,
        focus: false,
        capture: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--step" => args.step = it.next().and_then(|v| v.parse().ok()).expect("--step N"),
            "--name" => args.name = it.next().expect("--name NAME"),
            "--plan" => {
                args.plan = if it.next().as_deref() == Some("starter") {
                    Plan::Starter
                } else {
                    Plan::Pro
                }
            }
            "--focus" => args.focus = true,
            "--capture" => {
                args.capture =
                    Some(std::path::absolute(it.next().expect("--capture out.png")).unwrap())
            }
            other => panic!("unknown flag {other}"),
        }
    }
    args
}

/// Saves this window to `path` at scale 1 with niri's screenshot action (it also lands on the clipboard),
/// cropping the 640 × 560 client area the spec captures.
fn screenshot(path: &std::path::Path, scale: f32) -> Result<(), String> {
    let run = |cmd: &mut Command| cmd.output().map_err(|e| e.to_string());
    let windows =
        String::from_utf8_lossy(&run(Command::new("niri").args(["msg", "windows"]))?.stdout)
            .into_owned();
    let mut window = None;
    let mut found = None;
    for line in windows.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Window ID ") {
            window = rest.split(':').next().map(str::to_owned);
        } else if line == format!("PID: {}", std::process::id()) {
            found = window.clone();
        }
    }
    let id = found.ok_or("window not found in `niri msg windows`")?;
    let raw = path.with_extension("raw.png");
    let _ = std::fs::remove_file(&raw);
    run(Command::new("niri")
        .args(["msg", "action", "screenshot-window", "--id", &id, "--path"])
        .arg(&raw))?;
    for _ in 0..40 {
        if raw.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(100));
    let crop = format!("{}x{}+0+0", (640. * scale).round(), (560. * scale).round());
    let out = run(Command::new("magick")
        .arg(&raw)
        .args([
            "-crop", &crop, "+repage", "-filter", "Box", "-resize", "640x560!",
        ])
        .arg(path))?;
    let _ = std::fs::remove_file(&raw);
    out.status
        .success()
        .then_some(())
        .ok_or_else(|| String::from_utf8_lossy(&out.stderr).into_owned())
}

fn main() {
    let args = args();
    Application::new().run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                include_bytes!("../../../../assets/fonts/Inter-Regular.ttf")
                    .as_slice()
                    .into(),
                include_bytes!("../../../../assets/fonts/Inter-SemiBold.ttf")
                    .as_slice()
                    .into(),
            ])
            .expect("load Inter");
        input::bind_keys(cx);
        let capture = args.capture.is_some();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(640.), px(560.)),
                        cx,
                    ))),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Stepper · GPUI".into()),
                        ..Default::default()
                    }),
                    app_id: Some("stepper-gpui".into()),
                    ..Default::default()
                },
                |window, cx| {
                    let name = cx.new(|cx| {
                        let mut input =
                            TextInput::new(&args.name, "Your name", INK, PLACEHOLDER, cx);
                        input.hide_caret = capture;
                        input
                    });
                    if args.focus {
                        window.focus(&name.focus_handle(cx));
                    }
                    let step = args.step.clamp(1, TITLES.len() + 1);
                    cx.new(|cx| {
                        // Typing re-renders the stepper: Continue depends on the name.
                        cx.observe(&name, |_, _, cx| cx.notify()).detach();
                        Stepper {
                            current: step,
                            from_step: step,
                            leaving: None,
                            direction: 1.,
                            generation: 0,
                            glider_from: None,
                            viewport_from: None,
                            plan: Transition::new(args.plan),
                            field_focus: Transition::new(args.focus),
                            next_disabled: Transition::new(
                                step == 3 && args.name.trim().is_empty(),
                            ),
                            back_hidden: Transition::new(step <= 1),
                            completed: Transition::new(step > TITLES.len()),
                            back_hovered: false,
                            next_hovered: false,
                            back_hover: Transition::new(false),
                            next_hover: Transition::new(false),
                            name,
                            measured: Rc::default(),
                        }
                    })
                },
            )
            .expect("open window");
        cx.activate(true);
        if let Some(path) = args.capture.clone() {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1200))
                    .await;
                // Asked now: the output scale is only known once the surface is on screen.
                let scale = cx
                    .update(|cx| window.update(cx, |_, window, _| window.scale_factor()))
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or(1.);
                if let Err(e) = screenshot(&path, scale) {
                    eprintln!("capture failed: {e}");
                }
                cx.update(|cx| cx.quit()).ok();
            })
            .detach();
        }
    });
}
