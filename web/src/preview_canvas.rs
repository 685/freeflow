use crate::{
    dom::{self, all, id, string},
    state::{self, State},
};
use serde_json::{Value, json};
use wasm_bindgen::JsCast;
use web_sys::{KeyboardEvent, PointerEvent, WheelEvent};

const PREFERENCES: &str = "freeflow-preview-view";

#[derive(Clone, Default)]
pub struct Canvas {
    pub zoom: Option<f64>,
    pub pan: (f64, f64),
    pub panning: bool,
    pub brightness: String,
    drag: Option<(i32, f64, f64, f64, f64)>,
}

fn dimensions(state: &State) -> (f64, f64, f64) {
    match state.device.as_str() {
        "tablet" => (768.0, 930.0, 14.0),
        "desktop" => (1280.0, 760.0, 8.0),
        "expanded" => (
            expanded_size("width", id("canvas").client_width(), 32.0, 1280.0),
            expanded_size("height", id("canvas").client_height(), 96.0, 760.0),
            0.0,
        ),
        _ => (390.0, 780.0, 12.0),
    }
}

fn expanded_size(name: &str, size: i32, margin: f64, fallback: f64) -> f64 {
    if size > 0 {
        return (f64::from(size) - margin).max(1.0);
    }
    id("device-frame")
        .style()
        .get_property_value(name)
        .ok()
        .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(fallback)
}

fn scale(state: &State) -> f64 {
    let (width, height, bezel) = dimensions(state);
    state.canvas.zoom.unwrap_or_else(|| {
        if state.device == "expanded" {
            return 1.0;
        }
        ((f64::from(id("canvas").client_width()) - 46.0) / (width + bezel))
            .min((f64::from(id("canvas").client_height()) - 96.0) / (height + bezel))
            .clamp(0.12, 1.0)
    })
}

pub fn render(state: &State) {
    let (width, height, bezel) = dimensions(state);
    let scale = scale(state);
    let frame = id("device-frame");
    frame.set_class_name(&format!("device-frame {}", state.device));
    let style = frame.style();
    let _ = style.set_property("width", &format!("{}px", width + bezel));
    let _ = style.set_property("height", &format!("{}px", height + bezel));
    let _ = style.set_property("transform", &format!("scale({scale})"));
    let style = id("canvas-content").style();
    let _ = style.set_property("width", &format!("{}px", (width + bezel) * scale));
    let _ = style.set_property("height", &format!("{}px", (height + bezel) * scale));
    let _ = style.set_property(
        "transform",
        &format!(
            "translate(-50%, -50%) translate({}px, {}px)",
            state.canvas.pan.0, state.canvas.pan.1
        ),
    );
    let percent = (scale * 100.0).round() as u32;
    let fit_label = if state.canvas.zoom.is_none() {
        format!("Fit ({percent}%)")
    } else {
        "Fit".into()
    };
    let mut options = format!("<option value=\"fit\">{fit_label}</option>");
    let mut values = vec![30, 50, 75, 100, 125, 150, 200, 250, 300];
    if state.canvas.zoom.is_some() && !values.contains(&percent) {
        values.push(percent);
        values.sort_unstable();
    }
    for value in values {
        options.push_str(&format!("<option value=\"{value}\">{value}%</option>"));
    }
    let zoom = id("preview-zoom");
    if zoom.inner_html() != options {
        zoom.set_inner_html(&options);
    }
    dom::set_value(
        &zoom,
        &if state.canvas.zoom.is_some() {
            percent.to_string()
        } else {
            "fit".into()
        },
    );
    dom::disabled("preview-zoom-out", scale <= 0.3);
    dom::disabled("preview-zoom-in", scale >= 3.0);
    dom::attr(
        &id("preview-pan"),
        "aria-pressed",
        if state.canvas.panning {
            "true"
        } else {
            "false"
        },
    );
    dom::hidden("preview-pan-surface", !state.canvas.panning);
    dom::toggle(
        &id("preview-pan-surface"),
        "dragging",
        state.canvas.drag.is_some(),
    );
    let brightness = if state.canvas.brightness.is_empty() {
        "system"
    } else {
        &state.canvas.brightness
    };
    dom::set_value(&id("preview-brightness"), brightness);
    let compatible = state.inspect_ready || state.preview["status"] != "ready";
    dom::disabled("preview-brightness", !compatible);
    dom::attr(
        &id("preview-brightness"),
        "title",
        if compatible {
            "App brightness. Apps with a fixed theme keep their chosen mode."
        } else {
            "App brightness requires a compatible Flutter debug preview."
        },
    );
    for button in all("[data-device]") {
        let active = button.get_attribute("data-device").as_deref() == Some(&state.device);
        dom::toggle(&button, "active", active);
        dom::attr(
            &button,
            "aria-pressed",
            if active { "true" } else { "false" },
        );
    }
    let refreshing = if state.preview["result"]["reloading"] == true {
        " · Refreshing"
    } else {
        ""
    };
    dom::text(
        "canvas-info",
        &format!(
            "{} × {} · {percent}%{refreshing}",
            width.round(),
            height.round()
        ),
    );
}

fn persist(state: &State) {
    if let Ok(Some(storage)) = dom::win().local_storage() {
        let _ = storage.set_item(
            PREFERENCES,
            &json!({"zoom":state.canvas.zoom,"device":state.device,
            "brightness":state.canvas.brightness})
            .to_string(),
        );
    }
}

pub fn device(device: &str) {
    state::change(|state| {
        state.device = device.into();
        state.canvas.pan = (0.0, 0.0);
        render(state);
        persist(state);
    });
}

fn fit() {
    state::change(|state| {
        state.canvas.zoom = None;
        state.canvas.pan = (0.0, 0.0);
        render(state);
        persist(state);
    });
}

fn zoom(factor: f64, point: Option<(f64, f64)>) {
    if !factor.is_finite() || factor <= 0.0 {
        return;
    }
    state::change(|state| {
        let previous = scale(state);
        if previous <= 0.3 && factor <= 1.0 {
            return;
        }
        let next = ((previous * factor).clamp(0.3, 3.0) * 100.0).round() / 100.0;
        let rect = id("canvas").get_bounding_client_rect();
        let (x, y) = point.map_or((0.0, 0.0), |(x, y)| {
            (
                x - rect.left() - rect.width() / 2.0,
                y - rect.top() - rect.height() / 2.0,
            )
        });
        let (pan_x, pan_y) = state.canvas.pan;
        state.canvas.pan = (
            x - (x - pan_x) * next / previous,
            y - (y - pan_y) * next / previous,
        );
        state.canvas.zoom = Some(next);
        render(state);
        persist(state);
    });
}

pub fn wheel_from_preview(message: &Value) {
    let Some(delta) = message["delta"].as_f64().filter(|value| value.is_finite()) else {
        return;
    };
    let Some(x) = message["x"].as_f64().filter(|value| value.is_finite()) else {
        return;
    };
    let Some(y) = message["y"].as_f64().filter(|value| value.is_finite()) else {
        return;
    };
    let frame = id("preview-frame");
    if x < 0.0
        || y < 0.0
        || x > f64::from(frame.client_width())
        || y > f64::from(frame.client_height())
    {
        return;
    }
    let rect = frame.get_bounding_client_rect();
    let point = (
        rect.left() + x * rect.width() / f64::from(frame.client_width().max(1)),
        rect.top() + y * rect.height() / f64::from(frame.client_height().max(1)),
    );
    zoom(2.0_f64.powf((-delta / 600.0).clamp(-1.0, 1.0)), Some(point));
}

fn finish_drag() {
    state::change(|state| {
        state.canvas.drag = None;
        render(state);
    });
}

pub fn stop_pan(state: &mut State) {
    state.canvas.panning = false;
    state.canvas.drag = None;
}

pub fn brightness(value: &str) {
    if !matches!(value, "system" | "light" | "dark") {
        return;
    }
    state::change(|state| {
        state.canvas.brightness = value.into();
        persist(state);
        crate::preview_inspector::brightness(state);
        render(state);
    });
}

pub fn initialize() {
    let preferences = dom::win()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|storage| storage.get_item(PREFERENCES).ok().flatten())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    state::change(|state| {
        state.canvas.zoom = preferences["zoom"]
            .as_f64()
            .filter(|zoom| zoom.is_finite())
            .map(|zoom| zoom.clamp(0.3, 3.0));
        state.canvas.brightness = match string(&preferences["brightness"]) {
            "light" => "light",
            "dark" => "dark",
            _ => "system",
        }
        .into();
        state.device = match string(&preferences["device"]) {
            "tablet" => "tablet",
            "desktop" => "desktop",
            "expanded" => "expanded",
            _ => "phone",
        }
        .into();
    });
    dom::click("preview-zoom-out", || zoom(1.0 / 1.25, None));
    dom::click("preview-zoom-in", || zoom(1.25, None));
    dom::click("preview-fit", fit);
    dom::listen(&id("preview-zoom"), "change", |_| {
        let value = dom::value(&id("preview-zoom"));
        if value == "fit" {
            fit();
        } else if let Ok(percent) = value.parse::<f64>() {
            let current = state::read(scale);
            zoom(percent / 100.0 / current, None);
        }
    });
    dom::click("preview-pan", || {
        state::change(|state| {
            state.canvas.panning = !state.canvas.panning;
            state.canvas.drag = None;
            if state.canvas.panning {
                crate::preview_inspector::stop(state);
            }
            crate::preview_inspector::controls(state);
            if state.canvas.panning {
                let _ = id("preview-pan-surface").focus();
            }
        })
    });
    dom::listen(&id("preview-pan-surface"), "pointerdown", |event| {
        let Ok(event) = event.dyn_into::<PointerEvent>() else {
            return;
        };
        if !matches!(event.button(), 0 | 1) {
            return;
        }
        event.prevent_default();
        let _ = id("preview-pan-surface").set_pointer_capture(event.pointer_id());
        state::change(|state| {
            state.canvas.drag = Some((
                event.pointer_id(),
                f64::from(event.client_x()),
                f64::from(event.client_y()),
                state.canvas.pan.0,
                state.canvas.pan.1,
            ));
            render(state);
        });
    });
    dom::listen(&id("preview-pan-surface"), "pointermove", |event| {
        let Ok(event) = event.dyn_into::<PointerEvent>() else {
            return;
        };
        state::change(|state| {
            if let Some((pointer, x, y, pan_x, pan_y)) = state.canvas.drag {
                if pointer == event.pointer_id() {
                    state.canvas.pan = (
                        pan_x + f64::from(event.client_x()) - x,
                        pan_y + f64::from(event.client_y()) - y,
                    );
                    render(state);
                }
            }
        });
    });
    for name in ["pointerup", "pointercancel", "lostpointercapture"] {
        dom::listen(&id("preview-pan-surface"), name, |_| finish_drag());
    }
    dom::listen(&id("canvas"), "wheel", |event| {
        let Ok(event) = event.dyn_into::<WheelEvent>() else {
            return;
        };
        if event.ctrl_key() || event.meta_key() {
            event.prevent_default();
            zoom(
                2.0_f64.powf((-event.delta_y() / 600.0).clamp(-1.0, 1.0)),
                Some((f64::from(event.client_x()), f64::from(event.client_y()))),
            );
        }
    });
    dom::listen(&id("canvas"), "keydown", |event| {
        if event
            .dyn_ref::<KeyboardEvent>()
            .is_some_and(|key| key.key() == "Escape")
        {
            state::change(|state| {
                stop_pan(state);
                render(state);
            });
            return;
        }
        if dom::closest(&event, "input,select,button").is_some() {
            return;
        }
        let Ok(key) = event.dyn_into::<KeyboardEvent>() else {
            return;
        };
        match key.key().as_str() {
            "+" | "=" => {
                key.prevent_default();
                zoom(1.25, None);
            }
            "-" => {
                key.prevent_default();
                zoom(1.0 / 1.25, None);
            }
            "0" | "Home" => {
                key.prevent_default();
                fit();
            }
            direction if state::read(|state| state.canvas.panning) => {
                let (dx, dy) = match direction {
                    "ArrowLeft" => (-20.0, 0.0),
                    "ArrowRight" => (20.0, 0.0),
                    "ArrowUp" => (0.0, -20.0),
                    "ArrowDown" => (0.0, 20.0),
                    _ => return,
                };
                key.prevent_default();
                state::change(|state| {
                    state.canvas.pan.0 += dx;
                    state.canvas.pan.1 += dy;
                    render(state);
                });
            }
            _ => {}
        }
    });
    dom::listen(&dom::win(), "blur", |_| finish_drag());
    dom::listen(&id("preview-brightness"), "change", |_| {
        brightness(&dom::value(&id("preview-brightness")));
    });
}
