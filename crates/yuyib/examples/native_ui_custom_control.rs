//! Application-defined retained UI control with custom paint and drag behaviour.
//!
//! Run from the workspace root:
//!
//! ```text
//! cargo run -p yuyib --example native_ui_custom_control
//! ```
//!
//! The slider is not a hard-coded engine widget. `Widget::custom` supplies
//! layout, focus and pointer capture; `UiCustomPaintRegistry` draws it; and
//! `UiBehaviorRegistry` updates application-owned state from local pointer
//! coordinates. The same pattern supports custom scrollbars, node editors,
//! color pickers or composite editor controls.

use std::{cell::Cell, error::Error, rc::Rc};

use yuyib::{
    app::{Application, ApplicationUi, NativeUiTextConfig, RenderLoop},
    input::{UiDpiPolicy, WinitUiAdapter},
    platform::WindowConfig,
    render::ClearColor,
    ui::{
        Color, ColorToken, Dimension, Insets, LayoutConstraints, LayoutKind, Point, Size,
        UiBehaviorRegistry, UiBuilder, UiCustomEvent, UiCustomPointerEventKind, Widget, WidgetId,
        WidgetStyle,
    },
    ui_render::UiCustomPaintRegistry,
    ui_text::FontSource,
};

const DEV_FONT_FILE: &str = r"C:\Windows\Fonts\segoeui.ttf";
const VOLUME_SLIDER: WidgetId = WidgetId::new(0x9CA0_0051_1D3E_0001);
const SLIDER_RANGE: u16 = 1_000;
const SLIDER_THUMB_SIZE: u32 = 16;
const SLIDER_SIDE_PADDING: u32 = 14;

fn main() -> Result<(), Box<dyn Error>> {
    let value = Rc::new(Cell::new(SLIDER_RANGE / 2));
    let mut behaviours = UiBehaviorRegistry::default();
    let input_value = Rc::clone(&value);
    behaviours.on_event(VOLUME_SLIDER, move |event| {
        let UiCustomEvent::Pointer(pointer) = event else {
            return;
        };
        if !matches!(
            pointer.kind(),
            UiCustomPointerEventKind::Press | UiCustomPointerEventKind::Move
        ) {
            return;
        }
        let bounds = pointer.bounds();
        let track_width = bounds
            .size
            .width
            .saturating_sub(SLIDER_SIDE_PADDING.saturating_mul(2))
            .max(1);
        let track_start = i32::try_from(SLIDER_SIDE_PADDING).unwrap_or(i32::MAX);
        let track_end = track_start.saturating_add(i32::try_from(track_width).unwrap_or(i32::MAX));
        let local_x = pointer.local_position().x.clamp(track_start, track_end);
        let relative = u32::try_from(local_x.saturating_sub(track_start)).unwrap_or(0);
        let value =
            u16::try_from((u64::from(relative) * u64::from(SLIDER_RANGE)) / u64::from(track_width))
                .unwrap_or(SLIDER_RANGE);
        input_value.set(value.min(SLIDER_RANGE));
    });

    let mut painters = UiCustomPaintRegistry::default();
    let paint_value = Rc::clone(&value);
    painters.on_paint(VOLUME_SLIDER, move |context, canvas| {
        let bounds = context.bounds();
        let track_width = bounds
            .size
            .width
            .saturating_sub(SLIDER_SIDE_PADDING.saturating_mul(2))
            .max(1);
        let track_x = bounds
            .origin
            .x
            .saturating_add(i32::try_from(SLIDER_SIDE_PADDING).unwrap_or(i32::MAX));
        let track_y = bounds
            .origin
            .y
            .saturating_add(i32::try_from(bounds.size.height / 2).unwrap_or(i32::MAX))
            .saturating_sub(3);
        let filled_width = u32::try_from(
            (u64::from(track_width) * u64::from(paint_value.get())) / u64::from(SLIDER_RANGE),
        )
        .unwrap_or(track_width)
        .min(track_width);
        let thumb_travel = track_width.saturating_sub(SLIDER_THUMB_SIZE);
        let thumb_offset = u32::try_from(
            (u64::from(thumb_travel) * u64::from(paint_value.get())) / u64::from(SLIDER_RANGE),
        )
        .unwrap_or(thumb_travel)
        .min(thumb_travel);
        let thumb_color = if context.is_pressed() {
            Color::rgb(248, 178, 79)
        } else if context.is_hovered() {
            Color::rgb(124, 174, 255)
        } else {
            Color::rgb(92, 145, 255)
        };

        canvas.fill(Color::rgb(20, 28, 46));
        canvas.border(bounds, Color::rgb(63, 83, 120), 1);
        canvas.rectangle(
            yuyib::ui::Rect {
                origin: Point::new(track_x, track_y),
                size: Size::new(track_width, 6),
            },
            Color::rgb(51, 64, 88),
        );
        canvas.rectangle(
            yuyib::ui::Rect {
                origin: Point::new(track_x, track_y),
                size: Size::new(filled_width, 6),
            },
            Color::rgb(70, 132, 255),
        );
        canvas.rectangle(
            yuyib::ui::Rect {
                origin: Point::new(
                    track_x.saturating_add(i32::try_from(thumb_offset).unwrap_or(i32::MAX)),
                    bounds
                        .origin
                        .y
                        .saturating_add(i32::try_from(bounds.size.height / 2).unwrap_or(i32::MAX))
                        .saturating_sub(i32::try_from(SLIDER_THUMB_SIZE / 2).unwrap_or(i32::MAX)),
                ),
                size: Size::new(SLIDER_THUMB_SIZE, SLIDER_THUMB_SIZE),
            },
            thumb_color,
        );
    });

    let input = WinitUiAdapter::new(UiDpiPolicy::PhysicalPixels)?;
    let ui = ApplicationUi::new(custom_control_tree()?)
        .with_text(NativeUiTextConfig::new(FontSource::file(DEV_FONT_FILE)))?
        .with_behaviors(behaviours)
        .with_custom_paint(painters)
        .with_winit_input(input, |response| {
            for action in response.actions() {
                println!("UI: {action:?}");
            }
        });

    Application::new()
        .window(WindowConfig {
            title: "Yuyib — custom native UI control".to_owned(),
            width: 640,
            height: 250,
            ..Default::default()
        })
        .clear_color(ClearColor::linear(0.02, 0.03, 0.06, 1.0))
        .render_loop(RenderLoop::OnDemand)
        .ui(ui)
        .run()?;
    Ok(())
}

fn custom_control_tree() -> Result<yuyib::ui::UiTree, yuyib::ui::UiError> {
    let root = WidgetStyle::default()
        .with_background(ColorToken::Custom(Color::rgb(10, 16, 30)))
        .with_padding(Insets::all(24))
        .with_gap(12);
    let caption = WidgetStyle::default()
        .with_foreground(ColorToken::Text)
        .with_min_size(Size::new(0, 22));

    UiBuilder::new(
        WidgetId::from_key("custom-control-root"),
        LayoutKind::Column,
    )
    .with_style(root)
    .child(
        Widget::label(
            WidgetId::from_key("custom-control-title"),
            "Drag the bespoke volume control",
        )
        .with_style(caption),
    )
    .child(
        Widget::label(
            WidgetId::from_key("custom-control-note"),
            "This is Widget::custom: layout/input from Yuyib; paint + behavior from the app.",
        )
        .with_style(caption),
    )
    .child(
        Widget::custom(VOLUME_SLIDER, LayoutKind::Absolute).with_constraints(
            LayoutConstraints::auto()
                .with_width(Dimension::Fill)
                .with_height(Dimension::Points(48)),
        ),
    )
    .build()
}
