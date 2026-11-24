//
// rlr
//
// Copyright 2021 - Manos Pitsidianakis <manos@pitsidianak.is>
//
// This file is part of rlr.
//
// rlr is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// rlr is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with rlr. If not, see <http://www.gnu.org/licenses/>.
//
// SPDX-License-Identifier: GPL-3.0-or-later

#![deny(
    rustdoc::redundant_explicit_links,
    unsafe_op_in_unsafe_fn,
    /* groups */
    clippy::correctness,
    clippy::suspicious,
    clippy::complexity,
    clippy::perf,
    clippy::cargo,
    clippy::nursery,
    clippy::style,
    /* restriction */
    clippy::dbg_macro,
    clippy::rc_buffer,
    clippy::as_underscore,
    clippy::assertions_on_result_states,
    /* rustdoc */
    rustdoc::broken_intra_doc_links,
    /* pedantic */
    clippy::cast_lossless,
    clippy::cast_possible_wrap,
    clippy::ptr_as_ptr,
    clippy::doc_markdown,
    clippy::expect_fun_call,
    clippy::or_fun_call,
    clippy::bool_to_int_with_if,
    clippy::borrow_as_ptr,
    clippy::cast_ptr_alignment,
    clippy::large_futures,
    clippy::waker_clone_wake,
    clippy::unused_enumerate_index,
    clippy::unnecessary_fallible_conversions,
    clippy::struct_field_names,
    clippy::manual_hash_one,
    clippy::into_iter_without_iter,
)]
#![allow(
    // allow redundant_static_lifetimes to be able to compile from Rust version 1.70.0
    clippy::redundant_static_lifetimes,
    clippy::imprecise_flops,
    clippy::suboptimal_flops,
)]
use std::{
    f64::consts::{FRAC_PI_2, PI},
    io::Write,
    path::Path,
    rc::Rc,
    sync::Mutex,
};

use glib::{g_print, g_printerr};
use gtk::{
    cairo::{Context, FontSlant, FontWeight},
    gdk, gio, glib,
    prelude::*,
    AboutDialog, DrawingArea,
};

const APP_ID: &'static str = "com.github.epilys.rlr";

trait CairoContextExt {
    fn set_primary_color(&self, settings: &Settings);
    fn set_secondary_color(&self, settings: &Settings);
}

impl CairoContextExt for Context {
    fn set_primary_color(&self, settings: &Settings) {
        self.set_source_rgba(
            settings.primary_color.red(),
            settings.primary_color.green(),
            settings.primary_color.blue(),
            settings.primary_color.alpha(),
        );
    }

    fn set_secondary_color(&self, settings: &Settings) {
        self.set_source_rgba(
            settings.secondary_color.red(),
            settings.secondary_color.green(),
            settings.secondary_color.blue(),
            settings.secondary_color.alpha(),
        );
    }
}

const GSCHEMA_XML: &'static str =
    include_str!("../data/com.github.epilys.rlr.Settings.gschema.xml");

// include!("logo.xpm.rs");

/// Encode rotation state/angles around the starting left side as the origin
/// point.
///
/// ```text
///                   North
///                    ^^^
///                    |3|
///                    |2|
///               >    |1|
///              /     |0|      \
///            -/      +-+       \
///           /          .       v
///       <--------+.........+-------->
/// West  < 3 2 1 0|     .   |0 1 2 3 > East
///       <--------+     .   +-------->
///                      .        /
///             ^      +++      /-
///              \     |0|     <
///               \    |1|
///                    |2|
///                    |3|
///                    vvv
///
///                    South
/// ```
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
enum Rotation {
    E = 0,
    S = 1,
    W = 2,
    N = 3,
}

impl Rotation {
    #[inline(always)]
    const fn is_rotated(self) -> bool {
        !matches!(self as u8, 0 | 2)
    }

    #[inline(always)]
    const fn is_reversed(self) -> bool {
        matches!(self as u8, 2 | 3)
    }

    #[inline(always)]
    fn next(&mut self) -> Option<(Option<bool>, Option<bool>)> {
        use Rotation::*;
        let (new_val, ret) = match *self {
            E => (S, None),
            S => (W, Some((false.into(), None))),
            W => (N, Some((true.into(), false.into()))),
            N => (E, Some((None, true.into()))),
        };
        *self = new_val;
        ret
    }
}

#[derive(Clone, Copy, Debug)]
enum Interval {
    None,
    Start(f64),
    Full(f64, f64),
}

impl Interval {
    #[inline(always)]
    const fn is_start(&self) -> bool {
        matches!(self, Self::Start(_))
    }
}

/// Unit used for the ruler's ticks, labels and position readout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    /// Screen pixels.
    Pixels,
    /// Physical millimetres, derived from the monitor's reported size.
    Millimetres,
    /// Percentage of the ruler's length (the full length is 100%).
    Percent,
}

impl Unit {
    const fn next(self) -> Self {
        match self {
            Self::Pixels => Self::Millimetres,
            Self::Millimetres => Self::Percent,
            Self::Percent => Self::Pixels,
        }
    }

    /// Pixels per one unit along an axis of `axis_len` pixels.
    fn unit_px(self, axis_len: f64, px_per_mm: f64) -> f64 {
        match self {
            Self::Pixels => 1.0,
            Self::Millimetres => px_per_mm,
            Self::Percent => axis_len / 100.,
        }
    }
}

/// Unit used for the protractor's angle readout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AngleUnit {
    Degrees,
    Radians,
}

impl AngleUnit {
    const fn next(self) -> Self {
        match self {
            Self::Degrees => Self::Radians,
            Self::Radians => Self::Degrees,
        }
    }

    /// Format `angle` (in radians). Without `precision`, degrees snap to whole
    /// degrees and radians to whole radians.
    fn format(self, angle: f64, precision: bool) -> String {
        match self {
            Self::Radians if precision => format!("{angle:.3}rad"),
            Self::Radians => format!("{:.0}rad", angle.round()),
            Self::Degrees if precision => format!("{:.2}°", angle.to_degrees()),
            Self::Degrees => format!("{:.0}°", angle.to_degrees().round()),
        }
    }
}

/// Format a distance of `dist` pixels from the protractor's center. In percent
/// mode the radius `radius` is 100%.
fn format_distance(
    unit: Unit,
    dist: f64,
    radius: f64,
    px_per_mm: f64,
    scale_factor: i32,
    precision: bool,
) -> String {
    match unit {
        Unit::Pixels => format!("{:.0}px", dist.round() * f64::from(scale_factor)),
        Unit::Millimetres | Unit::Percent => {
            let (value, suffix) = if unit == Unit::Millimetres {
                (dist / px_per_mm, "mm")
            } else {
                (100. * dist / radius, "%")
            };
            if precision {
                format!("{value:.1}{suffix}")
            } else {
                format!("{:.0}{suffix}", value.round())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TickKind {
    Minor,
    Medium,
    Major,
}

#[derive(Clone, Debug, PartialEq)]
struct Tick {
    /// Pixel offset from the ruler's origin side (before flipping).
    pos: f64,
    kind: TickKind,
    label: Option<String>,
}

/// Compute the ticks of a ruler with an axis of `axis_len` pixels.
fn ruler_ticks(unit: Unit, axis_len: f64, px_per_mm: f64, scale_factor: i32) -> Vec<Tick> {
    let unit_px = unit.unit_px(axis_len, px_per_mm);
    if !(unit_px.is_finite() && unit_px > 0.) || axis_len <= 0. {
        return vec![];
    }
    // (minor step, medium step, major/labelled step) in units.
    let (minor, medium, major) = if unit == Unit::Pixels {
        (2_u32, 10_u32, 50_u32)
    } else {
        [(1, 5, 10), (2, 10, 20), (5, 25, 50), (10, 50, 100)]
            .into_iter()
            .find(|&(minor, _, major)| {
                f64::from(minor) * unit_px >= 3. && f64::from(major) * unit_px >= 30.
            })
            .unwrap_or((10, 50, 100))
    };
    let mut ticks = vec![];
    let mut k: u32 = 0;
    loop {
        let value = k * minor;
        let pos = f64::from(value) * unit_px;
        // Allow tiny floating point overshoot so that e.g. 100% is included.
        if pos > axis_len + 1e-6 {
            break;
        }
        let kind = if value % major == 0 {
            TickKind::Major
        } else if value % medium == 0 {
            TickKind::Medium
        } else {
            TickKind::Minor
        };
        let label = (kind == TickKind::Major).then(|| match unit {
            Unit::Pixels => format!("{}", i64::from(value) * i64::from(scale_factor)),
            Unit::Millimetres | Unit::Percent => format!("{value}"),
        });
        ticks.push(Tick { pos, kind, label });
        k += 1;
    }
    ticks
}

/// Compute where to draw the cursor line (pixel offset in window coordinates)
/// and the readout label for cursor pixel position `pos`.
fn ruler_readout(
    unit: Unit,
    pos: f64,
    axis_len: f64,
    px_per_mm: f64,
    scale_factor: i32,
    reversed: bool,
    precision: bool,
) -> (f64, String) {
    if unit == Unit::Pixels {
        let pos = if precision {
            pos.floor()
        } else {
            (pos / 10.).floor() * 10.
        };
        return (pos, format!("{}px", pos * f64::from(scale_factor)));
    }
    let unit_px = unit.unit_px(axis_len, px_per_mm);
    // Measure from the side where the zero tick is drawn.
    let offset = if reversed { axis_len - pos } else { pos };
    let value = offset / unit_px;
    let (value, offset) = if precision {
        (value, offset)
    } else {
        let v = value.round();
        (v, v * unit_px)
    };
    let draw_pos = if reversed { axis_len - offset } else { offset };
    let label = match (unit, precision) {
        (Unit::Millimetres, true) => format!("{value:.1}mm"),
        (Unit::Millimetres, false) => format!("{value:.0}mm"),
        (_, true) => format!("{value:.1}%"),
        (_, false) => format!("{value:.0}%"),
    };
    (draw_pos.floor(), label)
}

#[derive(Debug)]
struct Settings {
    obj: Option<gio::Settings>,
    primary_color: gdk::RGBA,
    secondary_color: gdk::RGBA,
    window_opacity: f64,
    font_size_factor: f64,
    font_name: String,
    window: Option<gtk::ApplicationWindow>,
    changed_signal_id: Option<glib::signal::SignalHandlerId>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            obj: None,
            primary_color: gdk::RGBA::parse("#453c0f").unwrap(),
            secondary_color: gdk::RGBA::parse("#f6d32d").unwrap(),
            window_opacity: 0.8,
            font_size_factor: 1.0,
            font_name: "Sans".to_string(),
            window: None,
            changed_signal_id: None,
        }
    }
}

impl Settings {
    const PRIMARY_COLOR: &'static str = "primary-color";
    const SECONDARY_COLOR: &'static str = "secondary-color";
    const WINDOW_OPACITY: &'static str = "window-opacity";
    const FONT_SIZE_FACTOR: &'static str = "font-size-factor";
    const FONT_NAME: &'static str = "font-name";
    const ALL_KEYS: &'static [(&'static str, &'static glib::VariantTy)] = &[
        (Self::PRIMARY_COLOR, glib::VariantTy::STRING),
        (Self::SECONDARY_COLOR, glib::VariantTy::STRING),
        (Self::WINDOW_OPACITY, glib::VariantTy::DOUBLE),
        (Self::FONT_SIZE_FACTOR, glib::VariantTy::DOUBLE),
        (Self::FONT_NAME, glib::VariantTy::STRING),
    ];

    fn new(schema_path: Option<&Path>) -> Result<Self, std::borrow::Cow<'static, str>> {
        let Some(default_schemas) = schema_path
            .and_then(|p| gio::SettingsSchemaSource::from_directory(p, None, true).ok())
            .or_else(gio::SettingsSchemaSource::default)
        else {
            return Err("Could not load default GSettings schemas.".into());
        };
        let Some(gsettings_schema) = default_schemas.lookup(APP_ID, true) else {
            return Err(format!("GSettings schema with id {APP_ID} was not found.").into());
        };
        let keys = gsettings_schema.list_keys();
        {
            let mut missing_keys = vec![];
            for (required_key, _) in Self::ALL_KEYS {
                if !keys.iter().any(|k| k == required_key) {
                    missing_keys.push(required_key);
                }
            }
            if !missing_keys.is_empty() {
                return Err(format!(
                    "GSettings schema does not contain valid keys; found keys {keys:?} but the \
                     following keys are missing: {missing_keys:?}."
                )
                .into());
            }
        }
        // Now that we have ensured the keys exist, we can look them up safely and check
        // that they have the correct data types.
        {
            let mut invalid_key_types = vec![];
            for (required_key, data_type) in Self::ALL_KEYS {
                let value_type = gsettings_schema.key(required_key).value_type();
                if value_type.as_ref() != *data_type {
                    invalid_key_types.push(format!(
                        "Expected {data_type} for key {required_key} but found {value_type} \
                         instead."
                    ));
                }
                if !invalid_key_types.is_empty() {
                    return Err(format!(
                        "GSettings schema contains invalid property types; the following errors \
                         were encountered:\n{}.",
                        invalid_key_types.join("\n")
                    )
                    .into());
                }
            }
        }
        let mut retval = Self::default();
        let settings = if schema_path.is_some() {
            gio::Settings::new_full(&gsettings_schema, gio::SettingsBackend::NONE, None)
        } else {
            gio::Settings::new(APP_ID)
        };
        retval.obj = Some(settings);
        retval.sync_read();
        Ok(retval)
    }

    fn sync_read(&mut self) {
        let Self {
            obj: Some(ref obj),
            ref mut primary_color,
            ref mut secondary_color,
            ref mut window_opacity,
            ref mut font_size_factor,
            ref mut font_name,
            window: _,
            changed_signal_id: _,
        } = self
        else {
            return;
        };
        let primary_color_s: String = obj.get(Self::PRIMARY_COLOR);
        if let Ok(val) = gdk::RGBA::parse(&primary_color_s) {
            *primary_color = val;
        } else {
            g_printerr!(
                "Invalid {} value: {:?}\n",
                Self::PRIMARY_COLOR,
                primary_color_s
            );
        }
        let secondary_color_s: String = obj.get(Self::SECONDARY_COLOR);
        if let Ok(val) = gdk::RGBA::parse(&secondary_color_s) {
            *secondary_color = val;
        } else {
            g_printerr!(
                "Invalid {} value: {:?}\n",
                Self::SECONDARY_COLOR,
                secondary_color_s
            );
        }
        *window_opacity = obj.get::<f64>(Self::WINDOW_OPACITY).clamp(0.01, 1.0);
        *font_size_factor = obj.get::<f64>(Self::FONT_SIZE_FACTOR).clamp(0.1, 10.0);
        *font_name = obj.get(Self::FONT_NAME);
    }

    fn sync_write(&self) {
        let Self {
            obj: Some(ref obj),
            ref primary_color,
            ref secondary_color,
            ref window_opacity,
            ref font_size_factor,
            ref font_name,
            ref changed_signal_id,
            window: _,
        } = self
        else {
            return;
        };
        if let Some(sid) = changed_signal_id.as_ref() {
            obj.block_signal(sid);
        }
        _ = obj.set(Self::PRIMARY_COLOR, primary_color.to_str().as_str());
        _ = obj.set(Self::SECONDARY_COLOR, secondary_color.to_str().as_str());
        _ = obj.set(Self::WINDOW_OPACITY, *window_opacity);
        _ = obj.set(Self::FONT_SIZE_FACTOR, *font_size_factor);
        _ = obj.set(Self::FONT_NAME, font_name);
        gio::Settings::sync();
        if let Some(sid) = changed_signal_id.as_ref() {
            obj.unblock_signal(sid);
        }
    }

    fn font_name(&self) -> &str {
        self.font_name
            .as_bytes()
            .iter()
            .rposition(|b| *b == b' ')
            .map_or_else(|| self.font_name.trim(), |sp| self.font_name[..sp].trim())
    }

    const fn is_installed(&self) -> bool {
        self.obj.is_some() && self.changed_signal_id.is_some()
    }

    fn try_install(also_compile: bool, path: &Path) -> Result<(), String> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Err(format!(
                "Directory {} either does not exist or you do not have permissions to access it.",
                path.display()
            ));
        };
        if !metadata.is_dir() {
            return Err(format!(
                "Argument value {} is not actually a directory.",
                path.display()
            ));
        }
        let gschema_path = path.join(format!("{APP_ID}.Settings.gschema.xml"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&gschema_path)
            .and_then(|mut file| file.write_all(GSCHEMA_XML.as_bytes()))
        {
            Err(err) => Err(format!(
                "Could not open {} for writing: {err}",
                path.display()
            )),
            Ok(_) => {
                if !also_compile {
                    return Ok(());
                }
                match std::process::Command::new("glib-compile-schemas")
                    .arg(path)
                    .stdin(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .stdin(std::process::Stdio::null())
                    .output()
                {
                    Ok(output) => {
                        if output.status.success() {
                            return Ok(());
                        }
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        Err(format!(
                            "`glib-compile-schemas {}` failed with exit status {}: stdout was: \
                             `{:?}`, stderr was `{:?}`",
                            path.display(),
                            output.status,
                            stdout,
                            stderr
                        ))
                    }
                    Err(err) => Err(format!(
                        "Could not launch `glib-compile-schemas {}`: {err}",
                        path.display()
                    )),
                }
            }
        }
    }

    fn set_window(rlr: Rc<Mutex<Rlr>>, window: gtk::ApplicationWindow) {
        let mut lck = rlr.lock().unwrap();
        lck.settings.window = Some(window.clone());
        lck.settings.changed_signal_id = lck.settings.obj.as_ref().map(|obj| {
            obj.connect_changed(
                None,
                glib::clone!(@strong rlr => move |_self: &gio::Settings, key: &str| {
                    let mut lck = rlr.lock().unwrap();
                    lck.settings.sync_read();
                    if key == Self::WINDOW_OPACITY {
                        window.set_opacity(lck.settings.window_opacity);
                    }
                    drop(lck);
                    window.queue_draw();
                }),
            )
        });
    }
}

#[derive(Debug)]
struct Rlr {
    position: (f64, f64),
    root_position: (i32, i32),
    width: i32,
    height: i32,
    p_dimens: Option<(i32, i32)>,
    freeze: bool,
    rotate: Rotation,
    protractor: bool,
    precision: bool,
    edit_angle_offset: bool,
    angle_offset: f64,
    interval: Interval,
    ppi: f64,
    /// Logical (unscaled) pixels per physical millimetre.
    px_per_mm: f64,
    unit: Unit,
    angle_unit: AngleUnit,
    scale_factor: i32,
    settings: Settings,
}

impl Default for Rlr {
    fn default() -> Self {
        let settings = match Settings::new(None) {
            Ok(settings) => settings,
            Err(error) => {
                g_printerr!("Could not load application settings. {error}\n");
                Settings::default()
            }
        };
        Self {
            position: (0., 0.),
            root_position: (0, 0),
            width: 500,
            height: 35,
            p_dimens: None,
            freeze: false,
            rotate: Rotation::E,
            protractor: false,
            precision: true,
            edit_angle_offset: false,
            angle_offset: 0.,
            interval: Interval::None,
            ppi: 72.,
            px_per_mm: 96. / 25.4,
            unit: Unit::Pixels,
            angle_unit: AngleUnit::Degrees,
            scale_factor: 1,
            settings,
        }
    }
}

fn draw_rlr(rlr: Rc<Mutex<Rlr>>, drar: &DrawingArea, cr: &Context) -> glib::Propagation {
    let lck = rlr.lock().unwrap();
    cr.set_font_size(
        lck.settings.font_size_factor * (8.0 / f64::from(lck.scale_factor)) * lck.ppi / 72.,
    );
    if lck.protractor {
        return lck.draw_douglas(drar, cr);
    }
    lck.draw_rlr(drar, cr)
}

impl Rlr {
    fn set_size(&self, window: &gtk::ApplicationWindow) {
        if self.protractor {
            let max = std::cmp::max(self.width, self.height);
            window.resize(max, max);
        } else {
            window.resize(self.width, self.height);
        }
    }

    fn calc_angle_of_point(&self, (xr, yr): (f64, f64)) -> f64 {
        if yr.abs() == 0. {
            if xr >= 0. {
                0.
            } else {
                PI
            }
        } else {
            2. * f64::atan(yr / (xr + (xr * xr + yr * yr).sqrt()))
        }
    }

    fn draw_douglas(&self, _drar: &DrawingArea, cr: &Context) -> glib::Propagation {
        let length: f64 = f64::from(std::cmp::min(self.width, self.height));
        let root_position = self.root_position;
        let root_position = (
            f64::from(root_position.0) - length / 2.,
            -(f64::from(root_position.1) - length / 2.),
        );
        let (xr, yr) = root_position;
        let angle = self.calc_angle_of_point((xr, yr));
        cr.arc(
            length / 2.,
            length / 2.,
            length / 2.,
            0.,
            2. * std::f64::consts::PI,
        );
        // Make entire canvas transparent, before starting to fill in the protractor
        // circular disk area which will be opaque.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.0);
        cr.fill().expect("Invalid cairo surface state");
        cr.set_secondary_color(&self.settings);
        cr.arc(
            length / 2.0,
            length / 2.0,
            length / 2.0,
            0.,
            2. * std::f64::consts::PI,
        );
        cr.fill().expect("Invalid cairo surface state");

        let _pixels_per_tick = 10;
        let tick_size = 5.;
        cr.set_primary_color(&self.settings);
        cr.set_line_width(1.);

        cr.save().unwrap();
        cr.set_source_rgba(0.1, 0.1, 0.1, 0.1);

        // Make concentric circles at the major ticks of the current distance unit
        for tick in ruler_ticks(self.unit, length / 2., self.px_per_mm, self.scale_factor) {
            let r = tick.pos;
            if tick.kind != TickKind::Major || r <= 0. || 2. * r >= length - 1. {
                continue;
            }
            cr.arc(length / 2., length / 2., r, 0., 2. * std::f64::consts::PI);
            cr.stroke().expect("Invalid cairo surface state");
        }
        cr.restore().unwrap();

        // Make circular angle ticks at the outmost circle
        for quadrant in 0..4 {
            let mut a: u64 = 0;
            // π * 0.5 == 1.57079...
            while a <= 157 {
                let tick_size = if ((a as f64) * (1.8 / PI)) % 30. <= 0.55 {
                    5.0 * tick_size
                } else if ((a as f64) * (1.8 / PI)) % 5. <= 0.5 {
                    1.5 * tick_size
                } else {
                    tick_size
                };
                cr.save().unwrap();
                cr.move_to(length / 2. - 0.5, length / 2. - 0.5);
                // cr.rotate(1.5 * PI + (quadrant as f64) * FRAC_PI_2);
                cr.rotate(f64::from(quadrant) * FRAC_PI_2);
                cr.rotate(-(a as f64 / 100.0));
                let cur = cr.current_point().unwrap();
                cr.move_to(cur.0 + length / 2. - 0.5 - tick_size, cur.1 - 0.5);
                cr.line_to(cur.0 + length / 2. - 0.5, cur.1 - 0.5);
                cr.stroke().expect("Invalid cairo surface state");
                cr.restore().unwrap();
                a += 1;
            }
        }

        // Make 0 radian radius (offsetted by `self.angle_offset`)
        cr.save().unwrap();
        cr.set_line_width(2.);
        cr.move_to(length / 2. - 0.5, length / 2. - 0.5);
        cr.rotate(2. * PI - FRAC_PI_2 - self.angle_offset);
        let cur = cr.current_point().unwrap();
        cr.line_to(cur.0, cur.1 + length / 2. - 0.5);
        cr.stroke().expect("Invalid cairo surface state");
        cr.restore().unwrap();

        // Draw radius tracking mouse position
        cr.save().unwrap();
        let _angle = if self.precision {
            angle + FRAC_PI_2
        } else {
            angle.round() + FRAC_PI_2
        };
        cr.move_to(length / 2. - 0.5, length / 2. - 0.5);
        cr.rotate(2. * PI - _angle);
        let cur = cr.current_point().unwrap();

        // Draw center point as a small circle
        cr.arc(cur.0, cur.1, 2., 0., 2. * std::f64::consts::PI);
        cr.stroke().expect("Invalid cairo surface state");
        cr.move_to(cur.0, cur.1);
        cr.line_to(cur.0, cur.1 + length / 2. - 0.5);
        cr.stroke().expect("Invalid cairo surface state");
        cr.restore().unwrap();
        cr.select_font_face(
            self.settings.font_name(),
            FontSlant::Normal,
            FontWeight::Normal,
        );

        // Draw arc signifying which angle is being measured
        cr.move_to(length / 2. - 0.5, length / 2. - 0.5);
        let angle = if root_position.1 < 0. {
            (PI - angle.abs()) + PI - self.angle_offset
        } else {
            angle - self.angle_offset
        };
        cr.arc(
            length / 2.,
            length / 2.,
            17.,
            2. * PI - _angle + FRAC_PI_2,
            2. * PI - self.angle_offset,
        );
        cr.stroke().expect("Invalid cairo surface state");

        // Show angle measurement as text
        cr.move_to(length / 2. - 5.5, length / 2. - 15.5);
        cr.show_text(&format!(
            " {}",
            self.angle_unit.format(angle, self.precision)
        ))
        .expect("Invalid cairo surface state");

        // Show distance of the pointer from the center as text below the center
        let dist = xr.hypot(yr);
        let dist_label = format!(
            " r = {}",
            format_distance(
                self.unit,
                dist,
                length / 2.,
                self.px_per_mm,
                self.scale_factor,
                self.precision,
            )
        );
        let extents = cr
            .text_extents(&dist_label)
            .expect("Invalid cairo surface state");
        cr.move_to(length / 2. - 5.5, length / 2. + 15.5 + extents.height());
        cr.show_text(&dist_label)
            .expect("Invalid cairo surface state");

        // Mark the pointer's distance on the tracking radius with a small circle
        if dist > 0. && dist < length / 2. {
            // Start a fresh path: `show_text` left the current point at the end of
            // the label, and `arc` would otherwise connect it with a line.
            cr.new_path();
            cr.arc(
                length / 2. + xr - 0.5,
                length / 2. - yr - 0.5,
                3.,
                0.,
                2. * std::f64::consts::PI,
            );
            cr.stroke().expect("Invalid cairo surface state");
        }

        glib::Propagation::Proceed
    }

    fn draw_rlr(&self, _drar: &DrawingArea, cr: &Context) -> glib::Propagation {
        let position = self.position;
        let length: f64 = f64::from(self.width);
        let height: f64 = f64::from(self.height);
        let breadth = if self.rotate.is_rotated() {
            f64::from(self.width)
        } else {
            f64::from(self.height)
        };

        cr.set_secondary_color(&self.settings);
        cr.paint().expect("Invalid cairo surface state");

        let _pixels_per_tick = 10;
        let tick_size = 5.;
        cr.set_line_width(0.5);
        cr.select_font_face(
            self.settings.font_name(),
            FontSlant::Normal,
            FontWeight::Normal,
        );
        cr.set_primary_color(&self.settings);
        cr.save().unwrap();
        match self.interval {
            Interval::Start(start_pos) => {
                cr.set_source_rgb(0.9, 0.9, 0.9);
                cr.rectangle(
                    start_pos - 0.5,
                    0.5,
                    position.0 - start_pos - 0.5,
                    breadth - 0.5,
                );
                cr.fill().expect("Invalid cairo surface state");
                cr.set_source_rgb(0.1, 0.1, 0.1);
                cr.rectangle(
                    start_pos - 0.5,
                    0.5,
                    position.0 - start_pos - 0.5,
                    breadth - 0.5,
                );
                cr.stroke().expect("Invalid cairo surface state");
            }
            Interval::Full(start_pos, end_pos) => {
                cr.set_source_rgb(0.8, 0.8, 0.8);
                cr.rectangle(
                    start_pos - 0.5,
                    0.5,
                    end_pos - 0.5 - start_pos,
                    breadth - 0.5,
                );
                cr.fill().expect("Invalid cairo surface state");
                cr.set_source_rgb(0.1, 0.1, 0.1);
                cr.rectangle(
                    start_pos - 0.5,
                    0.5,
                    end_pos - 0.5 - start_pos,
                    breadth - 0.5,
                );
                cr.stroke().expect("Invalid cairo surface state");
            }
            _ => {}
        }
        cr.restore().unwrap();
        cr.set_line_width(1.);
        cr.select_font_face(
            self.settings.font_name(),
            FontSlant::Normal,
            FontWeight::Normal,
        );
        let is_reversed = self.rotate.is_reversed();
        let is_rotated = self.rotate.is_rotated();
        let axis_len = if is_rotated { height } else { length };
        let ticks = ruler_ticks(self.unit, axis_len, self.px_per_mm, self.scale_factor);
        for tick in &ticks {
            // Keep ticks on the far edge (e.g. 100%) inside the window.
            let mut x = tick.pos.floor().min(axis_len - 1.).max(0.) + 0.5;
            if is_reversed {
                x = axis_len - x;
            }
            let tick_size = match tick.kind {
                TickKind::Major => tick_size * 1.5,
                TickKind::Medium => tick_size,
                TickKind::Minor => tick_size * 0.5,
            };
            if is_rotated {
                cr.move_to(1.0, x);
                cr.line_to(tick_size, x);
                cr.stroke().expect("Invalid cairo surface state");
                cr.move_to(breadth - tick_size, x);
                cr.line_to(breadth - 1.0, x);
            } else {
                cr.move_to(x, 1.0);
                cr.line_to(x, tick_size);
                cr.stroke().expect("Invalid cairo surface state");
                cr.move_to(x, breadth - tick_size);
                cr.line_to(x, breadth - 1.0);
            }
            cr.stroke().expect("Invalid cairo surface state");
            if let Some(label) = tick.label.as_deref() {
                let extents = cr.text_extents(label).expect("Invalid cairo surface state");
                if is_rotated {
                    let y = x.clamp(extents.height() + 1., (axis_len - 1.).max(1.));
                    cr.move_to(breadth / 2. - 2.5 - extents.width() / 2., y);
                } else {
                    let lx = (x - extents.width() / 2.)
                        .min(axis_len - extents.width() - 1.)
                        .max(1.);
                    cr.move_to(lx, breadth / 2. + 2.5);
                }
                cr.show_text(label).expect("Invalid cairo surface state");
            }
        }
        let (pos, pos_label) = ruler_readout(
            self.unit,
            if is_rotated { position.1 } else { position.0 },
            axis_len,
            self.px_per_mm,
            self.scale_factor,
            is_reversed,
            self.precision,
        );
        if is_rotated {
            let x = pos + 0.5;
            cr.move_to(1.0, x);
            cr.line_to(breadth, x);
            cr.stroke().expect("Invalid cairo surface state");
            let extents = cr
                .text_extents(&pos_label)
                .expect("Invalid cairo surface state");
            cr.rectangle(
                breadth / 2. - extents.width() / 2. - 2.,
                x - extents.height() - 2.,
                extents.width() + 6.5,
                extents.height() + 6.5,
            );
            cr.stroke().expect("Invalid cairo surface state");
            cr.rectangle(
                breadth / 2. - extents.width() / 2.,
                x - extents.height(),
                extents.width() + 4.5,
                extents.height() + 4.5,
            );
            cr.set_secondary_color(&self.settings);
            cr.fill().expect("Invalid cairo surface state");
            cr.set_primary_color(&self.settings);

            cr.move_to(breadth / 2. - extents.width() / 2., x);
            cr.show_text(&pos_label)
                .expect("Invalid cairo surface state");

            cr.rectangle(0.5, 0.5, length - 1.0, height - 1.0);
        } else {
            let x = pos + 0.5 + 2.0;
            cr.move_to(x - 2., 1.0);
            cr.line_to(x - 2., breadth);
            cr.stroke().expect("Invalid cairo surface state");

            let extents = cr
                .text_extents(&pos_label)
                .expect("Invalid cairo surface state");
            cr.rectangle(
                x - 2.,
                breadth / 2. - extents.height() - 2.,
                extents.width() + 6.5,
                extents.height() + 10.5,
            );
            cr.stroke().expect("Invalid cairo surface state");
            cr.rectangle(
                x,
                breadth / 2. - extents.height(),
                extents.width() + 4.5,
                extents.height() + 8.5,
            );
            cr.set_secondary_color(&self.settings);
            cr.fill().expect("Invalid cairo surface state");
            cr.set_primary_color(&self.settings);

            cr.move_to(x, breadth / 2. + 2.5);
            cr.show_text(&pos_label)
                .expect("Invalid cairo surface state");

            cr.rectangle(0.5, 0.5, length - 1.0, breadth - 1.0);
        }
        cr.stroke().expect("Invalid cairo surface state");

        glib::Propagation::Proceed
    }
}

fn run_app() -> Option<i32> {
    let application = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::default());

    let rlr = Rc::new(Mutex::new(Rlr::default()));

    application.add_main_option(
        "install-gsettings-schema",
        b'\0'.into(),
        glib::OptionFlags::NONE,
        glib::OptionArg::String,
        "Install the application's setting schema to the given directory. The directory will not \
         be created if it doesn't exist. As a special case, if the directory value is \"-\" the \
         schema will be printed at standard output. In most systems the value given should be one \
         of [\"$HOME/.local/share/glib-2.0/schemas/\", \"/usr/share/glib-2.0/schemas/\"]. As a \
         reminder, the command `glib-compile-schemas /path/to/glib-2.0/schemas/` must be executed \
         for changes to take effect.",
        Some("GLIB_2_0_SCHEMAS_DIR"),
    );
    application.connect_handle_local_options(
        |_: &gtk::Application, options_dict: &glib::VariantDict| -> i32 {
            if let Some(dir) = options_dict
                .lookup_value("install-gsettings-schema", Some(glib::VariantTy::STRING))
                .and_then(|variant| Some(variant.str()?.to_string()))
            {
                match dir.as_str() {
                    "-" => {
                        g_print!("{}", GSCHEMA_XML);
                        return 0;
                    }
                    actual_path => {
                        let path = Path::new(actual_path);
                        let Ok(metadata) = std::fs::metadata(path) else {
                            g_printerr!(
                                "Directory {} either does not exist or you do not have \
                                 permissions to access it.\n",
                                actual_path
                            );
                            return 1;
                        };
                        if !metadata.is_dir() {
                            g_printerr!(
                                "Argument value {} is not actually a directory.\n",
                                actual_path
                            );
                            return 1;
                        }
                        let gschema_path = path.join(format!("{APP_ID}.Settings.gschema.xml"));
                        match std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .open(&gschema_path)
                            .and_then(|mut file| file.write_all(GSCHEMA_XML.as_bytes()))
                        {
                            Err(err) => {
                                g_printerr!("Could not open {} for writing: {err}\n", actual_path);
                                return 1;
                            }
                            Ok(_) => {
                                g_print!(
                                    "Wrote schema to {}. You should run the following command to \
                                     compile the schema:\nglib-compile-schemas {actual_path}\n",
                                    gschema_path.display()
                                );
                            }
                        }
                        return 0;
                    }
                }
            }

            // Pretty print:
            //
            // g_printerr!("{:?}", options_dict.end().print(true));
            -1
        },
    );

    application.connect_startup(|application: &gtk::Application| {
        application.set_accels_for_action("app.quit", &["<Primary>Q", "Q"]);
        application.set_accels_for_action("app.rotate", &["R"]);
        application.set_accels_for_action("app.flip", &["<Shift>R"]);
        application.set_accels_for_action("app.protractor", &["P"]);
        application.set_accels_for_action("app.unit", &["U"]);
        application.set_accels_for_action("app.freeze", &["F", "space"]);
        application.set_accels_for_action("app.increase", &["plus"]);
        application.set_accels_for_action("app.decrease", &["minus"]);
        application.set_accels_for_action("app.increase_font_size", &["<Primary>plus"]);
        application.set_accels_for_action("app.decrease_font_size", &["<Primary>minus"]);
        application.set_accels_for_action("app.about", &["question", "F1"]);
        application.set_accels_for_action("app.settings", &["s", "F2"]);
        application
            .set_accels_for_action("app.move_right", &["Right", "<Primary>Right", "rightarrow"]);
        application.set_accels_for_action("app.move_left", &["Left", "<Primary>Left", "leftarrow"]);
        application.set_accels_for_action("app.move_up", &["Up", "<Primary>Up", "uparrow"]);
        application.set_accels_for_action("app.move_down", &["Down", "<Primary>Down", "downarrow"]);
        application.set_accels_for_action("app.move_to_center", &["Home", "h"]);
    });
    application.connect_activate(move |application: &gtk::Application| {
        let _rlr = rlr.clone();
        let _rlr2 = rlr.clone();
        drawable(
            application,
            _rlr,
            move |drar: &DrawingArea, cr: &Context| -> glib::Propagation {
                let _rlr = _rlr2.clone();
                draw_rlr(_rlr, drar, cr)
            },
        );
    });

    let retval = application.run();
    if retval != glib::ExitCode::SUCCESS {
        Some(retval.value())
    } else {
        None
    }
}

fn main() {
    gio::resources_register_include!("compiled.gresource").unwrap();

    if let Some(exit_code) = run_app() {
        std::process::exit(exit_code);
    }
}

fn drawable<F>(application: &gtk::Application, rlr: Rc<Mutex<Rlr>>, draw_fn: F)
where
    F: Fn(&DrawingArea, &Context) -> glib::Propagation + 'static,
{
    let window = gtk::ApplicationWindow::builder()
        .application(application)
        .events(gdk::EventMask::POINTER_MOTION_MASK)
        .build();
    window.set_icon(Some(
        &gtk::gdk_pixbuf::Pixbuf::from_resource(&format!("/images/{APP_ID}.svg")).unwrap(),
    ));

    set_visual(&window, None);

    {
        Settings::set_window(rlr.clone(), window.clone());
    }
    window.connect_screen_changed(set_visual);
    {
        let rlr = rlr.clone();
        let window = window.clone();
        let tick = move || {
            let mut lck = rlr.lock().unwrap();
            if lck.edit_angle_offset || lck.freeze {
                return glib::ControlFlow::Continue;
            }
            if let Some(screen) = window.window() {
                let root_origin = screen.root_origin();
                let Some(device) = screen
                    .display()
                    .default_seat()
                    .and_then(|seat| seat.pointer())
                else {
                    return glib::ControlFlow::Continue;
                };
                let (_, x, y) = device.position();
                let root_position = (x - root_origin.0, y - root_origin.1);

                if root_position != lck.root_position {
                    if lck.protractor {
                        lck.root_position = root_position;
                        lck.position.0 = f64::from(root_position.0);
                        lck.position.1 = f64::from(root_position.1);
                        drop(lck);
                        window.queue_draw();
                    } else if lck.rotate.is_rotated()
                        && root_position.1 < lck.height
                        && root_position.1 > 0
                    {
                        lck.root_position = root_position;
                        lck.position.1 = f64::from(root_position.1);
                        drop(lck);
                        window.queue_draw();
                    } else if !lck.rotate.is_rotated()
                        && root_position.0 < lck.width
                        && root_position.0 > 0
                    {
                        lck.root_position = root_position;
                        lck.position.0 = f64::from(root_position.0);
                        drop(lck);
                        window.queue_draw();
                    }
                }
            }
            glib::ControlFlow::Continue
        };

        // executes the closure once every second
        glib::timeout_add_local(std::time::Duration::from_millis(10), tick);
    }

    window.connect_enter_notify_event(enter_notify);
    window.connect_leave_notify_event(leave_notify);

    let accel_group = gtk::AccelGroup::new();
    window.add_accel_group(&accel_group);
    let menu = make_context_menu(&window, &accel_group);

    window.connect_button_press_event(glib::clone!(@strong rlr, @strong menu =>
    move |window: &gtk::ApplicationWindow, ev: &gtk::gdk::EventButton| {
        let mut lck = rlr.lock().unwrap();

        if matches!(ev.event_type(), gtk::gdk::EventType::ButtonPress)
            && lck.interval.is_start()
        {
            if let Interval::Start(start_pos) = lck.interval {
                lck.interval = Interval::Full(
                    start_pos,
                    if lck.rotate.is_rotated() {
                        ev.position().1
                    } else {
                        ev.position().0
                    },
                );
            }
        } else if matches!(ev.event_type(), gtk::gdk::EventType::DoubleButtonPress) {
            lck.interval = if lck.rotate.is_rotated() {
                Interval::Start(ev.position().1)
            } else {
                Interval::Start(ev.position().0)
            };
        } else if ev.button() == 1 && !lck.precision {
            lck.edit_angle_offset = true;
            drop(lck);
        } else if ev.button() == 1 {
            #[allow(clippy::cast_possible_wrap)]
            window.begin_move_drag(1, ev.root().0 as i32, ev.root().1 as i32, ev.time());
        } else if ev.button() == 3
            && matches!(ev.event_type(), gtk::gdk::EventType::ButtonPress)
        {
            menu.popup_at_pointer(Some(ev));
        }
        glib::Propagation::Proceed
    }));
    window.connect_button_release_event(
        glib::clone!(@strong rlr => move |_application: &gtk::ApplicationWindow, ev: &gtk::gdk::EventButton| {
                // g_printerr!("drag end\n");
                if ev.button() == 1 {
                    rlr.lock().unwrap().edit_angle_offset = false;
                }
                glib::Propagation::Proceed
            }
        ),
    );
    window.connect_key_press_event(
        glib::clone!(@strong rlr => move |window: &gtk::ApplicationWindow, ev: &gtk::gdk::EventKey| {
            // g_printerr!("press {}\n", ev.keyval().name().unwrap().as_str());
            if ev
                .keyval()
                .name()
                .map(|n| n.as_str() == "Control_L" || n.as_str() == "Meta_L")
                .unwrap_or(false)
            {
                rlr.lock().unwrap().precision = false;
                window.queue_draw();
            }
            glib::Propagation::Proceed
        }
    ));
    window.connect_key_release_event(
        glib::clone!(@strong rlr => move |window: &gtk::ApplicationWindow, ev: &gtk::gdk::EventKey| {
            // g_printerr!("release {}\n", ev.keyval().name().unwrap().as_str());
            if ev
                .keyval()
                .name()
                .map(|n| n.as_str() == "Control_L" || n.as_str() == "Meta_L")
                .unwrap_or(false)
            {
                rlr.lock().unwrap().precision = true;
                window.queue_draw();
            }
            glib::Propagation::Proceed
        }
    ));
    window.connect_motion_notify_event(
        glib::clone!(@strong rlr => move |window: &gtk::ApplicationWindow, motion: &gdk::EventMotion| {
            {
                let mut lck = rlr.lock().unwrap();
                if lck.freeze {
                    return glib::Propagation::Proceed;
                }
                lck.position = motion.position();
                if lck.edit_angle_offset {
                    let (xr, yr) = lck.position;
                    let translated_position = (
                        xr - f64::from(lck.width) / 2.,
                        f64::from(lck.width) / 2. - yr,
                    );
                    let angle = lck.calc_angle_of_point(translated_position);
                    lck.angle_offset = angle;
                }
            }
            window.queue_draw();
            glib::Propagation::Proceed
        }
    ));
    window.connect_configure_event(
        glib::clone!(@strong rlr => move |window: &gtk::ApplicationWindow, event: &gdk::EventConfigure| {
            {
                let mut lck = rlr.lock().unwrap();
                lck.width = event.size().0.try_into().unwrap_or(i32::MAX);
                lck.height = event.size().1.try_into().unwrap_or(i32::MAX);
            }
            window.queue_draw();

            false
        }
    ));
    window.set_app_paintable(true); // crucial for transparency
    window.set_resizable(true);
    window.set_decorated(false);

    // Run with GTK_DEBUG=interactive cargo run ... instead
    //
    // #[cfg(debug_assertions)]
    // gtk::Window::set_interactive_debugging(true);

    let drawing_area = DrawingArea::new();

    drawing_area.connect_draw(draw_fn);

    if let Ok(lck) = rlr.lock() {
        window.set_default_size(lck.width, lck.height);
    }

    window.add(&drawing_area);
    window.set_opacity(rlr.lock().unwrap().settings.window_opacity);

    add_actions(application, &window, rlr.clone());

    window.show_all();
    let (ppi, scale_factor) = get_ppi_and_scale_factor(&window);
    // The monitor may not report a physical size, in which case `ppi` is not finite.
    let ppi_valid = ppi.is_finite() && ppi > 0.;
    if let Ok(mut lck) = rlr.lock() {
        lck.px_per_mm = if ppi_valid {
            ppi / f64::from(scale_factor.max(1)) / 25.4
        } else {
            96. / 25.4
        };
        if ppi_valid && ppi > 72. {
            lck.ppi = ppi;
            lck.scale_factor = scale_factor;
            lck.width += (scale_factor * lck.width) / 2;
            lck.height += (scale_factor * lck.height) / 2;
            window.set_default_size(lck.width, lck.height);
            window.resize(lck.width, lck.height);
            window.queue_draw();
            // g_printerr!("resized to {}x{}\n", lck.width, lck.height);
        } else {
            lck.scale_factor = scale_factor;
        }
    }
}

fn get_ppi_and_scale_factor(window: &gtk::ApplicationWindow) -> (f64, i32) {
    const INCH: f64 = 0.0393701;

    let display = window.display();
    let monitor = display
        .monitor_at_window(&window.window().unwrap())
        .unwrap();
    let scale_factor = monitor.scale_factor();
    let width_mm = f64::from(monitor.width_mm());
    let height_mm = f64::from(monitor.height_mm());

    let rectangle = monitor.geometry();
    let width = f64::from(scale_factor) * f64::from(rectangle.width());
    let height = f64::from(scale_factor) * f64::from(rectangle.height());
    let diag = (width_mm * width_mm + height_mm * height_mm).sqrt() * INCH;

    (
        (width * width + height * height).sqrt() / diag,
        scale_factor,
    )
}

fn enter_notify(
    window: &gtk::ApplicationWindow,
    _crossing: &gtk::gdk::EventCrossing,
) -> glib::Propagation {
    // g_printerr!("enter\n");
    if let Some(screen) = window.window() {
        let display = screen.display();
        if let Some(gdk_window) = window.window() {
            gdk_window.set_cursor(Some(
                &gtk::gdk::Cursor::from_name(&display, "move").unwrap(),
            ));
        }
    }
    glib::Propagation::Proceed
}

const fn leave_notify(
    _application: &gtk::ApplicationWindow,
    _crossing: &gtk::gdk::EventCrossing,
) -> glib::Propagation {
    // g_printerr!("leave\n");
    glib::Propagation::Proceed
}

fn set_visual(window: &gtk::ApplicationWindow, _screen: Option<&gtk::gdk::Screen>) {
    if let Some(screen) = gtk::prelude::GtkWindowExt::screen(window) {
        if let Some(ref visual) = screen.rgba_visual() {
            window.set_visual(Some(visual)); // crucial for transparency
        }
    }
}

/// This function creates "actions" which connect on the declared actions from
/// the menu items.
fn add_actions(
    application: &gtk::Application,
    window: &gtk::ApplicationWindow,
    rlr: Rc<Mutex<Rlr>>,
) {
    let freeze = gio::SimpleAction::new("freeze", None);
    freeze.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            lck.freeze = !lck.freeze;
        }
        window.queue_draw();
    }));

    let flip = gio::SimpleAction::new("flip", None);
    flip.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            if !lck.protractor {
                let _ = lck.rotate.next();
                let _ = lck.rotate.next();
            }
        }
        window.queue_draw();
    }));

    let rotate = gio::SimpleAction::new("rotate", None);
    rotate.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            if !lck.protractor {
                let tmp = lck.width;
                lck.width = lck.height;
                lck.height = tmp;
                lck.set_size(&window);
                if let Some(direction) = lck.rotate.next() {
                    let (mut x, mut y) = window.position();
                    let (height, width) = (lck.height, lck.width);
                    drop(lck);
                    if let Some(dir_x) = direction.0 {
                        if dir_x {
                            x += height;
                        } else {
                            x = x.saturating_sub(width);
                            x = std::cmp::max(10, x);
                        }
                    }
                    if let Some(dir_y) = direction.1 {
                        if dir_y {
                            y += width;
                        } else {
                            y = y.saturating_sub(height);
                            y = std::cmp::max(10, y);
                        }
                    }
                    window.move_(x, y);
                }
            }
        }
        window.queue_draw();
    }));

    let unit = gio::SimpleAction::new("unit", None);
    unit.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            if lck.protractor {
                lck.angle_unit = lck.angle_unit.next();
            } else {
                lck.unit = lck.unit.next();
            }
        }
        window.queue_draw();
    }));

    let protractor = gio::SimpleAction::new("protractor", None);
    protractor.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            lck.protractor = !lck.protractor;
            if let Some((w, h)) = lck.p_dimens.take() {
                lck.p_dimens = Some((lck.width,lck.height ));
                lck.width = w;
                lck.height = h;
                window.resize(w, h);
            } else {
                lck.p_dimens = Some((lck.width,lck.height ));
                lck.set_size(&window);
            }

        }
        window.queue_draw();
    }));

    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(@weak window => move |_, _| {
        window.close();
    }));

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(@weak window => move |_, _| {
        show_about_window(&window);
    }));
    let settings = gio::SimpleAction::new("settings", None);
    settings.connect_activate(
        glib::clone!(@strong rlr, @weak application, @weak window => move |_, _| {
            show_settings_window(&application, &window, rlr.clone());
        }),
    );

    let increase = gio::SimpleAction::new("increase", None);
    increase.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            if !lck.protractor {
                if lck.rotate.is_rotated() {
                    lck.height += 50;
                } else {
                    lck.width += 50;
                }
            } else {
                lck.width += 50;
                lck.height = lck.width;
            }
            lck.set_size(&window);
        }
        window.queue_draw();
    }));
    let decrease = gio::SimpleAction::new("decrease", None);
    decrease.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            if !lck.protractor {
                if lck.rotate.is_rotated() {
                    lck.height -= 50;
                    lck.height = std::cmp::max(50, lck.height);
                } else {
                    lck.width -= 50;
                    lck.width = std::cmp::max(50, lck.width);
                }
            } else {
                lck.width -= 50;
                lck.width = std::cmp::max(50, lck.width);
                lck.height = lck.width;
            }
            lck.set_size(&window);
        }
        window.queue_draw();
    }));
    let increase_font_size = gio::SimpleAction::new("increase_font_size", None);
    increase_font_size.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            lck.settings.font_size_factor += 0.05;
            lck.settings.font_size_factor = lck.settings.font_size_factor.clamp(0.1, 10.0);
            lck.settings.sync_write();
        }
        window.queue_draw();
    }));
    let decrease_font_size = gio::SimpleAction::new("decrease_font_size", None);
    decrease_font_size.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        {
            let mut lck = rlr.lock().unwrap();
            lck.settings.font_size_factor -= 0.05;
            lck.settings.font_size_factor = lck.settings.font_size_factor.clamp(0.1, 10.0);
            lck.settings.sync_write();
        }
        window.queue_draw();
    }));
    let move_right = gio::SimpleAction::new("move_right", None);
    move_right.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        let precision = rlr.lock().unwrap().precision;
        let (mut x, y) = window.position();
        if !precision {
            x += 1;
        } else {
            x += 10;
        }
        window.move_(x, y);
        window.queue_draw();
    }));

    let move_left = gio::SimpleAction::new("move_left", None);
    move_left.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        let precision = rlr.lock().unwrap().precision;
        let (mut x, y) = window.position();
        if !precision {
            x -= 1;
        } else {
            x -= 10;
        }
        window.move_(x, y);
        window.queue_draw();
    }));

    let move_up = gio::SimpleAction::new("move_up", None);
    move_up.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        let precision = rlr.lock().unwrap().precision;
        let (x, mut y) = window.position();
        if !precision {
            y -= 1;
        } else {
            y -= 10;
        }
        window.move_(x, y);
        window.queue_draw();
    }));

    let move_down = gio::SimpleAction::new("move_down", None);
    move_down.connect_activate(glib::clone!(@strong rlr, @weak window => move |_, _| {
        let precision = rlr.lock().unwrap().precision;
        let (x, mut y) = window.position();
        if !precision {
            y += 1;
        } else {
            y += 10;
        }
        window.move_(x, y);
        window.queue_draw();
    }));

    // We need to add all the actions to the application so they can be taken into
    // account.

    application.add_action(&move_right);
    application.add_action(&move_left);
    application.add_action(&move_up);
    application.add_action(&move_down);
    application.add_action(&increase);
    application.add_action(&decrease);
    application.add_action(&increase_font_size);
    application.add_action(&decrease_font_size);
    application.add_action(&freeze);
    application.add_action(&protractor);
    application.add_action(&unit);
    application.add_action(&rotate);
    application.add_action(&flip);
    application.add_action(&about);
    application.add_action(&settings);
    application.add_action(&quit);
}

fn show_settings_window(
    application: &gtk::Application,
    window: &gtk::ApplicationWindow,
    rlr: Rc<Mutex<Rlr>>,
) {
    struct SettingsWidgets {
        primary_color_chooser: gtk::ColorButton,
        secondary_color_chooser: gtk::ColorButton,
        font_button: gtk::FontButton,
        opacity_adj: gtk::Adjustment,
        opacity_scale: gtk::Scale,
        font_size_adj: gtk::Adjustment,
        font_size_scale: gtk::Scale,
        info_label: std::cell::RefCell<Option<gtk::Label>>,
        try_install_button: std::cell::RefCell<Option<gtk::Widget>>,
    }
    let listbox = gtk::ListBox::builder()
        .visible(true)
        .sensitive(true)
        .can_focus(true)
        .expand(true)
        .build();
    let d = gtk::Dialog::builder()
        .application(application)
        .title("rlr Settings")
        .has_focus(true)
        .can_focus(true)
        .sensitive(true)
        .border_width(15)
        .resizable(false)
        .transient_for(window)
        .destroy_with_parent(true)
        .type_(gtk::WindowType::Toplevel)
        .type_hint(gdk::WindowTypeHint::Dialog)
        .build();
    let opacity_adj = gtk::Adjustment::new(0.0, 0.1, 1.1, 0.05, 0.1, 0.1);
    let font_size_adj = gtk::Adjustment::new(0.0, 0.1, 10.0, 0.05, 0.1, 0.1);
    let opacity_row = gtk::FlowBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .can_focus(true)
        .sensitive(true)
        .homogeneous(true)
        .expand(true)
        .visible(true)
        .max_children_per_line(2)
        .build();
    opacity_row.insert(&gtk::Label::new(Some("Opacity")), 0);
    let opacity_scale = gtk::Scale::builder()
        .can_focus(true)
        .sensitive(true)
        .visible(true)
        .digits(3)
        .adjustment(&opacity_adj)
        .expand(true)
        .build();
    opacity_row.insert(&opacity_scale, 1);
    let font_size_row = gtk::FlowBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .can_focus(true)
        .sensitive(true)
        .homogeneous(true)
        .expand(true)
        .visible(true)
        .max_children_per_line(2)
        .build();
    font_size_row.insert(&gtk::Label::new(Some("Font size factor")), 0);
    let font_size_scale = gtk::Scale::builder()
        .can_focus(true)
        .sensitive(true)
        .visible(true)
        .digits(3)
        .adjustment(&font_size_adj)
        .expand(true)
        .build();
    font_size_row.insert(&font_size_scale, 1);
    let primary_color_chooser = gtk::ColorButton::new();
    primary_color_chooser.set_expand(true);
    primary_color_chooser.set_use_alpha(true);
    let secondary_color_chooser = gtk::ColorButton::new();
    secondary_color_chooser.set_expand(true);
    secondary_color_chooser.set_use_alpha(true);
    let font_button = gtk::FontButton::new();
    font_button.set_level(gtk::FontChooserLevel::FAMILY | gtk::FontChooserLevel::STYLE);
    font_button.set_use_font(true);
    font_button.set_show_size(false);
    font_button.set_use_size(false);
    fn bind_settings(rlr: Rc<Mutex<Rlr>>, settings_widgets: &SettingsWidgets) -> bool {
        let lck = rlr.lock().unwrap();
        let SettingsWidgets {
            ref primary_color_chooser,
            ref secondary_color_chooser,
            ref font_button,
            ref opacity_adj,
            ref opacity_scale,
            ref font_size_adj,
            ref font_size_scale,
            ref info_label,
            ref try_install_button,
        } = settings_widgets;
        primary_color_chooser.set_rgba(&lck.settings.primary_color);
        secondary_color_chooser.set_rgba(&lck.settings.secondary_color);
        let is_gschema_installed = lck.settings.is_installed();
        macro_rules! _set_sens {
            ($($i:ident),*$(,)?) => {{ $($i.set_sensitive(is_gschema_installed);)* }};
        }
        _set_sens! {
            primary_color_chooser,
            secondary_color_chooser,
            font_button,
            opacity_scale,
            font_size_scale,
        };
        if let Some(gsettings_obj) = lck.settings.obj.as_ref() {
            font_button.set_font(lck.settings.font_name());
            if let Ok(r) = info_label.try_borrow() {
                if let Some(info_label) = r.as_ref() {
                    info_label.set_visible(false);
                    info_label.queue_draw();
                }
            }
            if let Ok(r) = try_install_button.try_borrow() {
                if let Some(btn) = r.as_ref() {
                    btn.set_visible(false);
                    btn.set_sensitive(false);
                    btn.queue_draw();
                }
            }
            gsettings_obj
                .bind(Settings::WINDOW_OPACITY, opacity_adj, "value")
                .build();
            gsettings_obj
                .bind(Settings::FONT_SIZE_FACTOR, font_size_adj, "value")
                .build();
            gsettings_obj
                .bind(Settings::PRIMARY_COLOR, primary_color_chooser, "rgba")
                .mapping(|var, _| {
                    let hash: String = var.get()?;
                    let val: gdk::RGBA = gdk::RGBA::parse(&hash).ok()?;
                    Some(val.into())
                })
                .set_mapping(|var, _| {
                    let val: gdk::RGBA = var.get().ok()?;
                    Some(val.to_str().to_string().into())
                })
                .build();
            gsettings_obj
                .bind(Settings::SECONDARY_COLOR, secondary_color_chooser, "rgba")
                .mapping(|var, _| {
                    let hash: String = var.get()?;
                    let val: gdk::RGBA = gdk::RGBA::parse(&hash).ok()?;
                    Some(val.into())
                })
                .set_mapping(|var, _| {
                    let val: gdk::RGBA = var.get().ok()?;
                    Some(val.to_str().to_string().into())
                })
                .build();
            gsettings_obj
                .bind(Settings::FONT_NAME, font_button, "font")
                .build();
        }
        drop(lck);
        is_gschema_installed
    }
    let settings_widgets = Rc::new(SettingsWidgets {
        primary_color_chooser,
        secondary_color_chooser,
        font_button,
        opacity_adj,
        opacity_scale,
        font_size_adj,
        font_size_scale,
        info_label: std::cell::RefCell::new(None),
        try_install_button: std::cell::RefCell::new(None),
    });
    let is_gschema_installed: bool = bind_settings(rlr.clone(), &settings_widgets);
    listbox.add(&opacity_row);
    listbox.add(&font_size_row);
    let font_name_row = gtk::FlowBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .can_focus(true)
        .sensitive(true)
        .homogeneous(true)
        .expand(true)
        .visible(true)
        .max_children_per_line(2)
        .build();
    font_name_row.insert(&gtk::Label::new(Some("Font")), 0);
    font_name_row.insert(&settings_widgets.font_button, 1);
    let primary_color_row = gtk::FlowBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .can_focus(true)
        .sensitive(true)
        .homogeneous(true)
        .expand(true)
        .visible(true)
        .max_children_per_line(2)
        .build();
    primary_color_row.insert(&gtk::Label::new(Some("Primary colour")), 0);
    primary_color_row.insert(&settings_widgets.primary_color_chooser, 1);
    listbox.add(&primary_color_row);
    let secondary_color_row = gtk::FlowBox::builder()
        .orientation(gtk::Orientation::Horizontal)
        .can_focus(true)
        .sensitive(true)
        .homogeneous(true)
        .expand(true)
        .visible(true)
        .max_children_per_line(2)
        .build();
    secondary_color_row.insert(&gtk::Label::new(Some("Secondary colour")), 0);
    secondary_color_row.insert(&settings_widgets.secondary_color_chooser, 1);
    listbox.add(&secondary_color_row);
    listbox.add(&font_name_row);
    if !is_gschema_installed {
        let label = gtk::Label::builder()
            .label(
                "<i>INFORMATION</i>: The <tt>GSettings</tt> XML schema does not seem to be \
                 installed on your system.\nYou can attempt to install it by clicking the <tt>Try \
                 install...</tt> button",
            )
            .use_markup(true)
            .sensitive(false)
            .visible(true)
            .expand(true)
            .build();
        if let Ok(mut guard) = settings_widgets.info_label.try_borrow_mut() {
            listbox.add(&label);
            *guard = Some(label);
        }
    }
    d.content_area().add(&listbox);
    d.content_area().set_visible(true);
    d.content_area().set_can_focus(true);
    d.add_button("Restore defaults", gtk::ResponseType::Reject);
    d.add_button("Close", gtk::ResponseType::Close);
    if !is_gschema_installed {
        if let Ok(mut guard) = settings_widgets.try_install_button.try_borrow_mut() {
            let btn = d.add_button("Try install...", gtk::ResponseType::Other(0));
            *guard = Some(btn);
        }
    }
    fn settings_response_handler(
        self_: &gtk::Dialog,
        application: &gtk::Application,
        window: &gtk::ApplicationWindow,
        rlr: Rc<Mutex<Rlr>>,
        settings_widgets: &SettingsWidgets,
        response: gtk::ResponseType,
    ) {
        match response {
            gtk::ResponseType::Reject => {
                let mut lck = rlr.lock().unwrap();
                lck.settings = Settings {
                    obj: lck.settings.obj.take(),
                    changed_signal_id: lck.settings.changed_signal_id.take(),
                    ..Settings::default()
                };
                lck.settings.sync_write();
                window.set_opacity(lck.settings.window_opacity);
                drop(lck);
                window.queue_draw();
            }
            gtk::ResponseType::Close => self_.emit_close(),
            gtk::ResponseType::Other(0) => {
                if let Some(home_dir) = glib::getenv("HOME") {
                    let destination_path =
                        Path::new(&home_dir).join(".local/share/glib-2.0/schemas/");
                    let md = gtk::MessageDialog::builder()
                        .title("Install application's settings schema")
                        .transient_for(self_)
                        .destroy_with_parent(true)
                        .modal(true)
                        .message_type(gtk::MessageType::Question)
                        .buttons(gtk::ButtonsType::OkCancel)
                        .text(format!(
                            "<tt>rlr</tt> will attempt to place its configuration schema to \
                             directory <tt>{}</tt>.\nIf the directory does not exist this will \
                             fail; you will have to create it manually.",
                            destination_path.display()
                        ))
                        .secondary_text(
                            "You can also perform this operation via the command line (see output \
                             of <tt>--help</tt>)",
                        )
                        .use_markup(true)
                        .application(application)
                        .icon_name("dialog-information")
                        .secondary_use_markup(true)
                        .decorated(true)
                        .build();
                    match md.run() {
                        gtk::ResponseType::Ok => {
                            if Settings::try_install(true, &destination_path)
                                .and_then(|_| {
                                    rlr.lock().unwrap().settings =
                                        Settings::new(Some(&destination_path))?;
                                    Settings::set_window(rlr.clone(), window.clone());
                                    Ok(())
                                })
                                .is_ok()
                            {
                                _ = bind_settings(rlr, settings_widgets);
                            }
                        }
                        gtk::ResponseType::Cancel => {}
                        _ => {}
                    }
                    window.queue_draw();
                    md.emit_close();
                }
            }
            _ => {}
        }
    }
    d.connect_response(glib::clone!(
            @strong settings_widgets,
            @strong listbox,
            @weak window,
            @weak application,
            @strong rlr => move |self_, response: gtk::ResponseType| {
                settings_response_handler(self_, &application, &window, rlr.clone(), &settings_widgets, response);
            }),
    );

    d.show_all();
}

fn show_about_window(window: &gtk::ApplicationWindow) {
    let gen_comments = |with_markup: bool| {
        format!(
            "{bs}Quit{be} with {ms}q{me} or {ms}{lt}{primary}{gt}Q{me}.
Click to {bs}drag{be}.
Press {ms}?{me} or {ms}F1{me} to {bs}open the About and Help window{be}.
Press {ms}s{me} or {ms}F2{me} to {bs}open the Settings window{be}.
Press {ms}r{me} to {bs}rotate{be} 90 degrees. Press {ms}{lt}Shift{gt}r{me} to {bs}flip \
             (mirror){be} the marks without rotation.
Press {ms}p{me} to toggle {bs}protractor mode{be}.
Press {ms}u{me} to {bs}cycle units{be}: in ruler mode between pixels, millimetres and percent of the ruler \
             length; in protractor mode between degrees and radians.
Press {ms}f{me} or {ms}{lt}Space{gt}{me} to toggle {bs}freezing the measurements{be}.
Press {ms}{primary}{me} and drag the angle base side to {bs}rotate it while in protractor mode{be}.
Press {ms}{primary}{me} continuously to {bs}disable precision{be} (measurements will snap to \
             nearest integer).
Press {ms}+{me} to {bs}increase size{be}. Press {ms}-{me} to {bs}decrease size{be}.
Press {ms}{lt}{primary}{gt}+{me}, {ms}{lt}{primary}{gt}+{me} to {bs}increase font size{be}. Press \
             {ms}{lt}{primary}{gt}-{me}, {ms}{lt}{primary}{gt}{me} to {bs}decrease font size{be}.
Press {ms}Up{me}, {ms}Down{me}, {ms}Left{me}, {ms}Right{me} to {bs}move window position by 10 \
             pixels{be}. Also hold down {ms}{primary}{me} to {bs}move by 1 pixel{be}.
",
            ms = if with_markup { "<tt>" } else { "`" },
            me = if with_markup { "</tt>" } else { "`" },
            lt = if with_markup { "&lt;" } else { "<" },
            gt = if with_markup { "&gt;" } else { ">" },
            primary = if cfg!(target_os = "macos") {
                "⌘"
            } else {
                "Control_L"
            },
            bs = if with_markup { "<b>" } else { "" },
            be = if with_markup { "</b>" } else { "" }
        )
    };
    let p = AboutDialog::new();
    p.set_program_name("rlr");
    p.set_logo(Some(
        &gtk::gdk_pixbuf::Pixbuf::from_resource(&format!("/images/{APP_ID}.svg")).unwrap(),
    ));
    p.set_website_label(Some("https://github.com/epilys/rlr"));
    p.set_website(Some("https://github.com/epilys/rlr"));
    p.set_authors(&["Manos Pitsidianakis <manos@pitsidianak.is>"]);
    p.set_copyright(Some("2021 - Manos Pitsidianakis"));
    p.set_title("About rlr");
    p.set_license_type(gtk::License::Gpl30);
    p.set_transient_for(Some(window));
    p.set_destroy_with_parent(true);
    p.set_comments(Some(&gen_comments(false)));
    // Access comments label widget through this monstrosity because AboutDialog
    // does not provide us with a clean interface to access the content widgets
    // semantically.
    if let Some(comments_widget) = p
        .content_area()
        .children()
        .first()
        .and_then(|w| w.clone().downcast::<gtk::Container>().ok())
        .and_then(|c| c.children().get(2).cloned())
        .and_then(|w| w.downcast::<gtk::Container>().ok())
        .and_then(|c| c.children().first().cloned())
        .and_then(|w| w.downcast::<gtk::Container>().ok())
        .and_then(|c| c.children().get(1).cloned())
    {
        if let Ok(comments_label) = comments_widget.downcast::<gtk::Label>() {
            if comments_label.text().starts_with("Quit") {
                comments_label.set_text(&gen_comments(true));
                comments_label.set_use_markup(true);
                comments_label.set_justify(gtk::Justification::Left);
            }
        }
    }
    p.connect_response(
        glib::clone!(@weak window => move |self_, response: gtk::ResponseType| {
            if matches!(response, gtk::ResponseType::Close | gtk::ResponseType::DeleteEvent) {
                self_.emit_close();
            }
        }),
    );
    p.show_all();
}

fn make_context_menu(window: &gtk::ApplicationWindow, accel_group: &gtk::AccelGroup) -> gtk::Menu {
    let mut menu = gtk::Menu::builder()
        .attach_widget(window)
        .accel_group(accel_group)
        .visible(true)
        .expand(true);
    macro_rules! add_child {
        ($(($label:literal, $action:literal)),*$(,)?) => {{
            $(menu = menu
                .child(
                    &{
                        let i = gtk::MenuItem::builder()
                            .label($label)
                            .action_name($action)
                            .visible(true)
                            .expand(true)
                            .build();
                        if let Some(accel_s) = window.application().and_then(|app| app.accels_for_action($action).get(0).cloned()) {
                            let (key, modifier) = gtk::accelerator_parse(&accel_s);

                            if let Some(al) = i.child().and_then(|c| c.downcast::<gtk::AccelLabel>().ok()) {
                                al.set_accel(key, modifier);
                            }
                        }
                        i
                    }
                );)*
        }};
        (@sep) => {{
            menu = menu.child(&gtk::SeparatorMenuItem::builder().visible(true).build());
        }};
    }
    add_child! {
        ("Rotate", "app.rotate"),
        ("Flip", "app.flip"),
        ("Toggle protractor", "app.protractor"),
        ("Cycle units", "app.unit"),
        ("Toggle freeze", "app.freeze"),
        ("Increase size", "app.increase"),
        ("Decrease size", "app.decrease"),
        ("Increase font size", "app.increase_font_size"),
        ("Decrease font size", "app.decrease_font_size"),
    }
    add_child! {
        @sep
    };
    add_child! {
        ("Settings", "app.settings"),
        ("About", "app.about"),
    };
    add_child! {
        @sep
    };
    add_child! {
        ("Quit", "app.quit"),
    };
    menu.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_ticks_match_previous_layout() {
        let ticks = ruler_ticks(Unit::Pixels, 500., 96. / 25.4, 1);
        assert_eq!(ticks.len(), 251);
        assert_eq!(ticks[0].label.as_deref(), Some("0"));
        assert_eq!(ticks[5].kind, TickKind::Medium);
        assert_eq!(ticks[25].label.as_deref(), Some("50"));
        let ticks = ruler_ticks(Unit::Pixels, 100., 1., 2);
        assert_eq!(ticks[25].label.as_deref(), Some("100"));
    }

    #[test]
    fn percent_ticks_span_full_length() {
        for len in [123., 500., 777., 2000.] {
            let ticks = ruler_ticks(Unit::Percent, len, 1., 1);
            let last = ticks.last().unwrap();
            assert!((last.pos - len).abs() < 1e-6, "len {len}");
            assert_eq!(last.label.as_deref(), Some("100"));
        }
    }

    #[test]
    fn millimetre_ticks() {
        let ppm = 96. / 25.4;
        let ticks = ruler_ticks(Unit::Millimetres, 500., ppm, 1);
        assert_eq!(ticks[10].label.as_deref(), Some("10"));
        assert!((ticks[10].pos - 10. * ppm).abs() < 1e-9);
        assert!(ticks.last().unwrap().pos <= 500.);
    }

    #[test]
    fn angle_formats() {
        let a = std::f64::consts::FRAC_PI_4;
        assert_eq!(AngleUnit::Degrees.format(a, true), "45.00°");
        assert_eq!(AngleUnit::Degrees.format(0.7, false), "40°");
        assert_eq!(AngleUnit::Radians.format(a, true), "0.785rad");
    }

    #[test]
    fn distance_formats() {
        assert_eq!(
            format_distance(Unit::Pixels, 50.4, 200., 4., 2, true),
            "100px"
        );
        assert_eq!(
            format_distance(Unit::Millimetres, 40., 200., 4., 1, true),
            "10.0mm"
        );
        assert_eq!(
            format_distance(Unit::Percent, 50., 200., 4., 1, true),
            "25.0%"
        );
        assert_eq!(
            format_distance(Unit::Percent, 51., 200., 4., 1, false),
            "26%"
        );
    }

    #[test]
    fn readouts() {
        assert_eq!(
            ruler_readout(Unit::Percent, 250., 500., 1., 1, false, true).1,
            "50.0%"
        );
        // Flipped: the zero tick is on the far side.
        assert_eq!(
            ruler_readout(Unit::Percent, 100., 500., 1., 1, true, true).1,
            "80.0%"
        );
        assert_eq!(
            ruler_readout(Unit::Millimetres, 40., 500., 4., 1, false, true).1,
            "10.0mm"
        );
        let (pos, label) = ruler_readout(Unit::Percent, 252., 500., 1., 1, false, false);
        assert_eq!((pos, label.as_str()), (250., "50%"));
        assert_eq!(
            ruler_readout(Unit::Pixels, 123.7, 500., 1., 2, false, true).1,
            "246px"
        );
    }
}
